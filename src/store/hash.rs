use super::{Entry, Pointer};
use equivalent::Equivalent;
use std::{hash::Hash, ops::Deref};

mod papaya;
mod sync;

pub use sync::FixedCapSyncHashStore;

pub trait Store<T: Value>: super::Store<T> {
    #[inline]
    fn keys(&self) -> impl Iterator<Item = impl Deref<Target = T::Key>> {
        self.iter().map(ExtractKey)
    }

    fn entry<'a, K>(
        &'a self,
        key: &'a K,
    ) -> Entry<
        impl OccupiedEntry<'a, Value = T, Pointer = Self::Pointer> + use<'a, Self, T, K>,
        impl VacantEntry<'a, Value = T, Pointer = Self::Pointer> + use<'a, Self, T, K>,
    >
    where
        K: ?Sized + Hash + Equivalent<T::Key>;

    #[inline]
    fn get(&self, key: &(impl ?Sized + Hash + Equivalent<T::Key>)) -> Option<Self::Pointer> {
        match self.entry(key) {
            Entry::Occupied(o) => Some(o.into_pointer()),
            Entry::Vacant(_) => None,
        }
    }

    #[inline]
    fn get_many<K, const N: usize>(&self, keys: [&K; N]) -> [Option<Self::Pointer>; N]
    where
        K: ?Sized + Hash + Equivalent<T::Key>,
    {
        std::array::from_fn(|i| self.get(keys[i]))
    }

    fn or_insert_with<K>(&self, key: K, value: impl FnOnce(K) -> T) -> Self::Pointer
    where
        K: Hash + Equivalent<T::Key>;
    // {
    //     let entry = self.entry(&key);
    //     match entry {
    //         Entry::Occupied(o) => o.into_pointer(),
    //         Entry::Vacant(v) => match v.try_insert(value(key)) {
    //             Ok(p) => p,
    //             Err((_v, o)) => o.into_pointer(),
    //         },
    //     }
    // }

    #[inline]
    fn remove_key(&self, key: &(impl ?Sized + Hash + Equivalent<T::Key>)) -> Option<Self::Pointer> {
        self.remove_key_if(key, |_| true).ok().unwrap()
    }

    #[inline]
    fn remove_key_if(
        &self,
        key: &(impl ?Sized + Hash + Equivalent<T::Key>),
        mut f: impl FnMut(&Self::Pointer) -> bool,
    ) -> Result<Option<Self::Pointer>, Self::Pointer> {
        match self.entry(key) {
            Entry::Occupied(o) if f(o.pointer()) => Ok(o.remove_key()),
            Entry::Occupied(o) => Err(o.into_pointer()),
            Entry::Vacant(_) => Ok(None)
        }
    }
}

pub trait Value {
    type Key: ?Sized + Eq + Hash;

    fn key(&self) -> &Self::Key;
}

pub trait OccupiedEntry<'a>: Sized + 'a {
    type Value: Value;
    type Pointer: Pointer<Target = Self::Value>;
    type VacantEntry: VacantEntry<'a, Value = Self::Value, Pointer = Self::Pointer, OccupiedEntry = Self>;

    fn get(&self) -> &Self::Value;

    fn pointer(&self) -> &Self::Pointer;

    fn into_pointer(self) -> Self::Pointer;

    fn try_remove(self) -> Result<Self::Pointer, Entry<Self, Self::VacantEntry>>;

    #[inline]
    fn remove_key(self) -> Option<Self::Pointer> {
        let mut this = self;
        loop {
            match this.try_remove() {
                Ok(p) => return Some(p),
                Err(Entry::Occupied(occupied)) => this = occupied,
                Err(Entry::Vacant(_)) => return None,
            }
        }
    }

    fn try_insert(
        self,
        value: Self::Value,
    ) -> Result<Self::Pointer, (Self::Value, Entry<Self, Self::VacantEntry>)>;

    #[inline]
    fn insert(self, value: Self::Value) -> Self::Pointer {
        let mut this = self;
        let mut that = value;
        loop {
            match this.try_insert(that) {
                Ok(p) => return p,
                Err((value, Entry::Occupied(occupied))) => {
                    this = occupied;
                    that = value;
                }
                Err((value, Entry::Vacant(vacant))) => match vacant.try_insert(value) {
                    Ok(p) => return p,
                    Err((value, occupied)) => {
                        this = occupied;
                        that = value;
                    }
                },
            }
        }
    }
}

pub trait VacantEntry<'a>: Sized {
    type Value: Value;
    type Pointer: Pointer<Target = Self::Value>;
    type OccupiedEntry: OccupiedEntry<'a, Value = Self::Value, Pointer = Self::Pointer>;

    fn try_insert(
        self,
        value: Self::Value,
    ) -> Result<Self::Pointer, (Self::Value, Self::OccupiedEntry)>;

    #[inline]
    fn insert(self, value: Self::Value) -> Self::Pointer {
        match self.try_insert(value) {
            Ok(p) => p,
            Err((value, occupied)) => occupied.insert(value),
        }
    }
}

#[derive(Clone, Copy)]
struct ExtractKey<P>(P);

impl<P: Deref> Deref for ExtractKey<P>
where
    P::Target: Value,
{
    type Target = <P::Target as Value>::Key;

    fn deref(&self) -> &Self::Target {
        self.0.key()
    }
}

// #[inline]
// pub(crate) fn impl_insert<T, S>(store: &S, value: T) -> S::Pointer
// where
//     T: Value,
//     S: Store<T>,
// {
//     match store.entry(value.key()) {
//         Entry::Occupied(o) => o.insert(value),
//         Entry::Vacant(v) => v.insert(value),
//     }
// }

// #[inline]
// pub(crate) fn impl_or_insert<T, S>(store: &S, value: T) -> S::Pointer
// where
//     T: Value,
//     S: Store<T>,
// {
//     match store.entry(value.key()) {
//         Entry::Occupied(o) => o.into_pointer(),
//         Entry::Vacant(v) => v.insert(value),
//     }
// }

// #[inline]
// pub(crate) fn impl_remove<T, S>(store: &S, value: &T) -> Option<S::Pointer>
// where
//     T: Value,
//     S: Store<T>,
// {
//     store.remove_key(value.key())
// }

// #[inline]
// pub(crate) fn impl_upsert<T, S>(
//     store: &S,
//     value: T,
//     f: impl FnOnce(T, &S::Pointer) -> Option<T>,
// ) -> S::Pointer
// where
//     T: Value,
//     S: Store<T>,
// {
//     match store.entry(value.key()) {
//         Entry::Occupied(o) => {
//             if let Some(replacement) = f(value, o.pointer()) {
//                 o.insert(replacement)
//             } else {
//                 o.into_pointer()
//             }
//         }
//         Entry::Vacant(v) => v.insert(value),
//     }
// }
