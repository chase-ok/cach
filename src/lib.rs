use std::{borrow::Borrow, hash::Hash, marker::PhantomData, ops::Deref};

pub mod evict;
pub mod map;
pub mod sharded;
mod time;
use map::MapCache;
pub use time::{Clock, DefaultClock};
pub mod expire;
// pub mod sync;

pub trait Cache<T: Value> {
    type Pointer: Deref<Target = T> + Clone;

    fn len(&self) -> usize;

    fn entry<'c, 'k, K>(
        &'c self,
        key: &'k K,
    ) -> Entry<
        impl OccupiedEntry<Pointer = Self::Pointer> + 'c,
        impl VacantEntry<Pointer = Self::Pointer> + 'c,
    >
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq;

    fn insert(&self, value: T) -> Self::Pointer {
        match self.entry(value.key()) {
            Entry::Occupied(o) => o.replace(value),
            Entry::Vacant(v) => v.insert(value),
        }
    }

    fn upsert(&self, value: T, f: impl FnOnce(T, &T) -> Option<T>) -> Self::Pointer {
        match self.entry(value.key()) {
            Entry::Occupied(o) => {
                if let Some(replacement) = f(value, o.value()) {
                    o.replace(replacement)
                } else {
                    o.into_pointer()
                }
            }
            Entry::Vacant(v) => v.insert(value),
        }
    }

    fn or_insert(&self, value: T) -> Self::Pointer {
        self.entry(value.key()).or_insert(value)
    }

    fn or_insert_with<K>(&self, key: &K, f: impl FnOnce() -> T) -> Self::Pointer
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        self.entry(key).or_insert_with(f)
    }

    fn or_insert_default<K>(&self, key: &K) -> Self::Pointer
    where
        T: Default,
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        self.or_insert_with(key, Default::default)
    }

    fn remove_if<K: ?Sized>(&self, key: &K, f: impl FnOnce(&T) -> bool) -> Option<Self::Pointer>
    where
        T::Key: Borrow<K>,
        K: Hash + Eq,
    {
        match self.entry(key) {
            Entry::Occupied(o) if f(o.value()) => Some(o.remove()),
            _ => None,
        }
    }

    fn remove<K: ?Sized>(&self, key: &K) -> Option<Self::Pointer>
    where
        T::Key: Borrow<K>,
        K: Hash + Eq,
    {
        self.remove_if(key, |_existing| true)
    }

    // XX good to override if can avoid write lock
    fn get<K: ?Sized>(&self, key: &K) -> Option<Self::Pointer>
    where
        T::Key: Borrow<K>,
        K: Hash + Eq,
    {
        match self.entry(key) {
            Entry::Occupied(o) => Some(o.into_pointer()),
            Entry::Vacant(_) => None,
        }
    }
}

pub enum Entry<O, V> {
    Occupied(O),
    Vacant(V),
}

pub trait OccupiedEntry: Sized {
    type Pointer: Deref;

    fn value(&self) -> &<Self::Pointer as Deref>::Target;

    fn pointer(&self) -> Self::Pointer;

    fn into_pointer(self) -> Self::Pointer {
        self.pointer()
    }

    fn replace(self, value: <Self::Pointer as Deref>::Target) -> Self::Pointer
    where
        <Self::Pointer as Deref>::Target: Sized;

    fn remove(self) -> Self::Pointer;
}

pub trait VacantEntry {
    type Pointer: Deref;

    fn insert(self, value: <Self::Pointer as Deref>::Target) -> Self::Pointer
    where
        <Self::Pointer as Deref>::Target: Sized;
}

impl<O: OccupiedEntry, V: VacantEntry<Pointer = O::Pointer>> Entry<O, V> {
    pub fn or_insert_with(self, f: impl FnOnce() -> <O::Pointer as Deref>::Target) -> O::Pointer
    where
        <O::Pointer as Deref>::Target: Sized,
    {
        match self {
            Entry::Occupied(o) => o.into_pointer(),
            Entry::Vacant(v) => v.insert(f()),
        }
    }

    pub fn or_insert(self, value: <O::Pointer as Deref>::Target) -> O::Pointer
    where
        <O::Pointer as Deref>::Target: Sized,
    {
        self.or_insert_with(|| value)
    }

    pub fn or_insert_default(self) -> O::Pointer
    where
        <O::Pointer as Deref>::Target: Default,
    {
        self.or_insert_with(Default::default)
    }
}

pub trait Value {
    type Key: ?Sized + Hash + Eq;

    fn key(&self) -> &Self::Key;
}

pub trait BuildCache<T: Value> {
    fn build(self) -> impl Cache<T>;
}

pub trait BuildCacheExt<T: Value>: Sized + BuildCache<T> {
    fn intrusive_expiring(self) -> impl BuildCache<T>
    where
        T: expire::Expire,
    {
        expire::IntrusiveExpireCacheBuilder::new(self)
    }

    fn expire_after_write_intrusive(self) -> impl BuildCache<T> {
        self
        // IntrusiveExpiryTimeCache {
        //     inner: todo!(),
        //     clock: todo!(),
        // }
    }
}

impl<T: Value, C: BuildCache<T> + Sized> BuildCacheExt<T> for C {}

pub trait BuildMapCacheExt<K: Eq + Hash, V>: Sized + BuildCache<map::MapEntry<K, V>> {
    fn build_map_cache(self) -> MapCache<K, V, Self> {}
}

impl<K: Eq + Hash, V, C: BuildCache<map::MapEntry<K, V>> + Sized> BuildMapCacheExt<K, V> for C {}

struct CachePointerFn<C, P, F> {
    cache: C,
    _pointer: PhantomData<P>,
    pointer_fn: F,
}

impl<C, P, F> CachePointerFn<C, P, F> {
    pub(crate) fn new(cache: C, pointer_fn: F) -> Self {
        Self {
            cache,
            _pointer: PhantomData,
            pointer_fn,
        }
    }
}

impl<T, C, P, F> Cache<T> for CachePointerFn<C, P, F>
where
    T: Value,
    C: Cache<T>,
    P: Deref<Target = T> + Clone,
    F: Fn(C::Pointer) -> P,
{
    type Pointer = P;

    fn len(&self) -> usize {
        self.cache.len()
    }

    fn entry<'c, 'k, K>(
        &'c self,
        key: &'k K,
    ) -> Entry<
        impl OccupiedEntry<Pointer = Self::Pointer> + 'c,
        impl VacantEntry<Pointer = Self::Pointer> + 'c,
    >
    where
        <T as Value>::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        match self.cache.entry(key) {
            Entry::Occupied(occupied) => {
                struct Occupied<'c, O, P, F> {
                    occupied: O,
                    _pointer: PhantomData<P>,
                    pointer_fn: &'c F,
                }

                impl<O, P, F> OccupiedEntry for Occupied<'_, O, P, F>
                where
                    O: OccupiedEntry,
                    P: Deref,
                    F: Fn(O::Pointer) -> P,
                {
                    type Pointer = P;

                    fn value(&self) -> &<Self::Pointer as Deref>::Target {
                        self.occupied.value()
                    }

                    fn pointer(&self) -> Self::Pointer {
                        (self.pointer_fn)(self.occupied.pointer())
                    }

                    fn into_pointer(self) -> Self::Pointer {
                        (self.pointer_fn)(self.occupied.into_pointer())
                    }

                    fn replace(self, value: <Self::Pointer as Deref>::Target) -> Self::Pointer
                    where
                        <Self::Pointer as Deref>::Target: Sized,
                    {
                        (self.pointer_fn)(self.occupied.replace(value))
                    }

                    fn remove(self) -> Self::Pointer {
                        (self.pointer_fn)(self.occupied.remove())
                    }
                }

                Entry::Occupied(Occupied {
                    occupied,
                    _pointer: PhantomData,
                    pointer_fn: &self.pointer_fn,
                })
            }
            Entry::Vacant(vacant) => {
                struct Vacant<'c, V, P, F> {
                    vacant: V,
                    _pointer: PhantomData<P>,
                    pointer_fn: &'c F,
                }

                impl<O, P, F> VacantEntry for Vacant<'_, O, P, F>
                where
                    O: VacantEntry,
                    P: Deref,
                    F: Fn(O::Pointer) -> P,
                {
                    type Pointer = P;
                    
                    fn insert(self, value: <Self::Pointer as Deref>::Target) -> Self::Pointer
                    where
                        <Self::Pointer as Deref>::Target: Sized 
                    {
                        (self.pointer_fn)(self.vacant.insert(value))
                    }

                }

                Entry::Vacant(Vacant {
                    vacant,
                    _pointer: PhantomData,
                    pointer_fn: &self.pointer_fn,
                })
            }
        }
    }

    fn get<K: ?Sized>(&self, key: &K) -> Option<Self::Pointer>
    where
        <T as Value>::Key: Borrow<K>,
        K: Hash + Eq,
    {
        self.cache.get(key).map(self.pointer_fn)
    }
}
