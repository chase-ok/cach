use std::{borrow::Borrow, hash::Hash, ops::Deref, time::Instant};

use crate::{SharedPointer, Value};

mod sync;
// mod scc;

pub trait Cache<T: Value> {
    type Pointer: SharedPointer<T>;

    fn len(&self) -> usize;

    fn iter(&self) -> impl Iterator<Item = Self::Pointer>;

    // fn entry<'a, 'k, K>(
    //     &'a self,
    //     key: &'k K,
    // ) -> Entry<
    //     impl Occupied<Value = T, Pointer = Self::Pointer> + 'a,
    //     impl Vacant<Value = T, Pointer = Self::Pointer> + 'a,
    // >
    // where
    //     T::Key: Borrow<K>,
    //     K: ?Sized + Hash + Eq;

    fn compute<R>(
        &self,
        value: T,
        f: impl FnMut(Option<T>, Option<&Self::Pointer>) -> Mutate<T, R>,
    ) -> Compute<Self::Pointer, R>;

    fn compute_key<K, R>(
        &self,
        key: &K,
        f: impl FnMut(Option<T>, Option<&Self::Pointer>) -> Mutate<T, R>,
    ) -> Compute<Self::Pointer, R>
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq;

    fn get<K>(&self, key: &K) -> Option<Self::Pointer>
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        match self.compute_key(key, |_, current| Mutate::None(current.cloned())) {
            Compute::None(p) => p,
            _ => unreachable!(),
        }
    }

    fn insert(&self, value: T) -> Self::Pointer {
        let Ok(pointer) = self.insert_if(value, |_, _| true) else {
            unreachable!()
        };
        pointer
    }

    fn insert_if(
        &self,
        value: T,
        mut f: impl FnMut(&T, Option<&T>) -> bool,
    ) -> Result<Self::Pointer, (T, Option<Self::Pointer>)> {
        let compute = self.compute(value, move |value, current| {
            let value = value.unwrap();
            if f(&value, current.map(|c| &**c)) {
                Mutate::Insert(value)
            } else {
                Mutate::None((value, current.cloned()))
            }
        });

        match compute {
            Compute::None(r) => Err(r),
            Compute::Inserted(p) => Ok(p),
            Compute::Overwrote { after, .. } => Ok(after),
            _ => unreachable!(),
        }
    }

    fn remove_if<K: ?Sized>(
        &self,
        key: &K,
        mut f: impl FnMut(&T) -> bool,
    ) -> Result<Self::Pointer, Option<Self::Pointer>>
    where
        T::Key: Borrow<K>,
        K: Hash + Eq,
    {
        let compute = self.compute_key(key, |_, current| {
            match current {
                Some(value) if f(value) => Mutate::Remove,
                _ => Mutate::None(current.cloned())
            }
        });
        match compute {
            Compute::None(p) => Err(p),
            Compute::Removed(p) => Ok(p),
            _ => unreachable!()
        }
    }

    fn remove<K>(&self, key: &K) -> Option<Self::Pointer>
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        match self.remove_if(key, |_| true) {
            Ok(p) => Some(p),
            Err(None) => None,
            _ => unreachable!()
        }
    }

    fn or_insert(&self, value: T) -> Self::Pointer {
        let compute = self.compute(value, |value, current| {
            if let Some(current) = current {
                Mutate::None(current.clone())
            } else {
                Mutate::Insert(value.unwrap())
            }
        });
        match compute {
            Compute::None(p) => p,
            Compute::Inserted(p) => p,
            _ => unreachable!()
        }
    }

    fn or_insert_with<K>(&self, key: &K, f: impl FnOnce() -> T) -> Self::Pointer
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        let mut f = Some(f);
        let compute = self.compute_key(key, |value, current| {
            if let Some(current) = current {
                Mutate::None(current.clone())
            } else {
                Mutate::Insert(value.unwrap_or_else(|| f.take().unwrap()()))
            }
        });

        match compute {
            Compute::None(p) => p,
            Compute::Inserted(p) => p,
            _ => unreachable!(),
        }
    }

    fn or_insert_default<K>(&self, key: &K) -> Self::Pointer
    where
        T: Default,
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        self.or_insert_with(key, Default::default)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mutate<T, R = ()> {
    None(R),
    Insert(T),
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Compute<P, R = ()> {
    None(R),
    Inserted(P),
    Overwrote { before: P, after: P },
    Removed(P),
    Err(ComputeError),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ComputeError {
    message: &'static str
}

pub trait Builder {
    type Cache<T: Value>: Cache<T>;

    fn build<T: Value>(self) -> Self::Cache<T>;
}

pub trait Layer {
    type Builder<B: Builder>: Builder;

    fn apply<B: Builder>(self, builder: B) -> Self::Builder<B>;
}

struct ExpireAfterWrite {

}

impl Layer for ExpireAfterWrite {
    type Builder<B: Builder> = ExpireAfterWriteBuilder<B>;

    fn apply<B: Builder>(self, builder: B) -> Self::Builder<B> {
        ExpireAfterWriteBuilder(builder)
    }
}

struct ExpireAfterWriteBuilder<B>(B);

impl<B: Builder> Builder for ExpireAfterWriteBuilder<B> {
    type Cache<T: Value> = ExpireAfterWriteCache<B::Cache<ExpireAfterWriteValue<T>>>;

    fn build<T: Value>(self) -> Self::Cache<T> {
        ExpireAfterWriteCache(self.0.build())
    }
}

struct ExpireAfterWriteCache<C>(C);

struct ExpireAfterWriteValue<T> {
    value: T,
    expire: Instant,
}

impl<T: Value> Value for ExpireAfterWriteValue<T> {
    type Key = T::Key;

    fn key(&self) -> &Self::Key {
        todo!()
    }
}

impl<C: Cache<ExpireAfterWriteValue<T>>, T: Value> Cache<T> for ExpireAfterWriteCache<C> {
    type Pointer = ExpireAfterWritePointer<C::Pointer>;

    fn len(&self) -> usize {
        todo!()
    }

    fn iter(&self) -> impl Iterator<Item = Self::Pointer> {
        self.0.iter().map(ExpireAfterWritePointer)
    }

    fn compute<R>(
        &self,
        value: T,
        mut f: impl FnMut(Option<T>, Option<&Self::Pointer>) -> Mutate<T, R>,
    ) -> Compute<Self::Pointer, R> {
        let now = Instant::now();
        let value = ExpireAfterWriteValue { value, expire: now };
        let compute = self.0.compute(value, move |value, current| {
            match current {
                Some(current) if current.expire > Instant::now() => {
                    match f(value.map(|v| v.value), None) {
                        Mutate::None(r) => Mutate::Remove, // XX need to store r!
                        Mutate::Insert(value) => Mutate::Insert(ExpireAfterWriteValue { value, expire: now }),
                        Mutate::Remove => Mutate::Remove,
                    }
                },
                Some(current) => todo!(),
                None => match f(value.map(|v| v.value), None) {
                    Mutate::None(r) => Mutate::None(r),
                    Mutate::Insert(value) => Mutate::Insert(ExpireAfterWriteValue { value, expire: now }),
                    Mutate::Remove => Mutate::Remove,
                }
            }
        });
        todo!()
    }

    fn compute_key<K, R>(
        &self,
        key: &K,
        f: impl FnMut(Option<T>, Option<&Self::Pointer>) -> Mutate<T, R>,
    ) -> Compute<Self::Pointer, R>
    where
        <T as Value>::Key: Borrow<K>,
        K: ?Sized + Hash + Eq {
        todo!()
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
#[repr(transparent)]
struct ExpireAfterWritePointer<P>(P);

impl<P: Deref<Target = ExpireAfterWriteValue<T>>, T> Deref for ExpireAfterWritePointer<P> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0.value
    }
}


// pub trait Occupied: Sized {
//     type Value;
//     type Pointer: Deref<Target = Self::Value>;

//     fn value(&self) -> &Self::Value;

//     fn pointer(&self) -> &Self::Pointer;

//     fn into_pointer(self) -> Self::Pointer;

//     fn try_replace(
//         self,
//         value: Self::Value,
//     ) -> Result<
//         Self::Pointer,
//         (
//             Self::Value,
//             Entry<Self, impl Vacant<Value = Self::Value, Pointer = Self::Pointer>>,
//         ),
//     >;

//     fn try_remove(
//         self,
//     ) -> Result<Self::Pointer, Entry<Self, impl Vacant<Value = Self::Value, Pointer = Self::Pointer>>>;
// }

// pub trait Vacant {
//     type Value;
//     type Pointer: Deref<Target = Self::Value>;

//     fn try_insert(
//         self,
//         value: Self::Value,
//     ) -> Result<Self::Pointer, (Self::Value, impl Occupied<Pointer = Self::Pointer>)>;
// }
