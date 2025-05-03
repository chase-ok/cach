use std::{borrow::Borrow, hash::Hash, ops::Deref};

use crate::{Entry, SharedPointer, Value};

pub trait Cache<T: Value> {
    type Pointer: SharedPointer<T>;

    fn len(&self) -> usize;

    fn iter(&self) -> impl Iterator<Item = Self::Pointer>;

    fn get<K>(&self, key: &K) -> Option<Self::Pointer>
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq;

    fn try_put(
        &self,
        value: T,
        expected: Option<&T>,
    ) -> Result<Self::Pointer, (T, Option<Self::Pointer>)>;

    fn try_remove<K>(&self, key: &K, expected: &T) -> Result<Self::Pointer, Option<Self::Pointer>>
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq;

    fn put(&self, mut value: T) -> Self::Pointer {
        let mut expected = None;
        loop {
            match self.try_put(value, expected.as_ref().map(Deref::deref)) {
                Ok(p) => return p,
                Err((v, e)) => {
                    value = v;
                    expected = e;
                }
            }
        }
    }

    fn try_replace(&self, value: T, expected: &T) -> Result<Self::Pointer, T> {
        self.try_put(value, Some(expected)).map_err(|(v, _e)| v)
    }

    fn remove<K>(&self, key: &K) -> Option<Self::Pointer>
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        self.remove_if(key, |_| true)
    }

    fn remove_if<K: ?Sized>(&self, key: &K, mut f: impl FnMut(&T) -> bool) -> Option<Self::Pointer>
    where
        T::Key: Borrow<K>,
        K: Hash + Eq,
    {
        let mut expected = self.get(key)?;
        while f(&expected) {
            match self.try_remove(key, &expected) {
                Ok(p) => return Some(p),
                Err(None) => return None,
                Err(Some(e)) => expected = e,
            }
        }
        None
    }

    fn or_insert(&self, mut value: T) -> Self::Pointer {
        loop {
            match self.try_put(value, None) {
                Ok(p) => return p,
                Err((_v, Some(e))) => return e,
                Err((v, None)) => value = v,
            }
        }
    }

    fn or_insert_with<K>(&self, key: &K, f: impl FnOnce() -> T) -> Self::Pointer
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        match self.get(key) {
            Some(p) => p,
            None => self.or_insert(f()),
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

    fn entry<'a, 'k, K>(
        &'a self,
        key: &'k K,
    ) -> Entry<impl Occupied<Pointer = Self::Pointer> + 'a, impl Vacant<Pointer = Self::Pointer> + 'a>
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq;
}

pub trait Occupied: Sized {
    type Pointer: Deref;

    fn value(&self) -> &<Self::Pointer as Deref>::Target;

    fn pointer(&self) -> Self::Pointer;

    fn into_pointer(self) -> Self::Pointer {
        self.pointer()
    }

    // fn replace(self, value: <Self::Pointer as Deref>::Target) -> Self::Pointer
    // where
    //     <Self::Pointer as Deref>::Target: Sized;

    // fn remove(self) -> Self::Pointer;
}

pub trait Vacant {
    type Pointer: Deref;

    // fn insert(self, value: <Self::Pointer as Deref>::Target) -> Self::Pointer
    // where
    //     <Self::Pointer as Deref>::Target: Sized;
}
