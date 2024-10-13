use std::sync::Arc;

use crate::{Cache, Value};


pub struct AtomicCache<T> {
    map: papaya::HashMap<Arc<T>, ()>,
}

impl<T: Value> Cache<T> for AtomicCache<T> {
    type Pointer = Arc<T>;

    const PREFER_LOCKED: bool = false;

    fn len(&self) -> usize {
        self.map.pin().len()
    }

    fn entry<'c, 'k, K>(&'c self, key: &'k K) -> impl crate::Entry<Pointer = Self::Pointer> + 'c
    where
        <T as Value>::Key: std::borrow::Borrow<K>,
        K: ?Sized + std::hash::Hash + Eq + ToOwned<Owned = <T as Value>::Key> {
        todo!()
    }

    fn locked_entry<'c, 'k, K>(
        &'c self,
        key: &'k K,
    ) -> crate::LockedEntry<
        impl crate::LockedOccupiedEntry<Pointer = Self::Pointer> + 'c,
        impl crate::LockedVacantEntry<Pointer = Self::Pointer> + 'c,
    >
    where
        <T as Value>::Key: std::borrow::Borrow<K>,
        K: ?Sized + std::hash::Hash + Eq {
        todo!()
    }
}