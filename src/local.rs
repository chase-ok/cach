use std::borrow::Borrow;
use std::{cell::RefCell, hash::BuildHasher, marker::PhantomData, ops::Deref, rc::Rc};

use hashbrown::raw::{Bucket, InsertSlot};
use hashbrown::{hash_map::DefaultHashBuilder, raw::RawTable};
use smallvec::SmallVec;
use std::hash::Hash;

use crate::{
    build::BuildCache,
    evict::{Eviction, NoEviction},
};

#[derive(Debug, Clone)]
pub struct LocalCacheBuilder<E = NoEviction, Ev = (), Eq = (), S = DefaultHashBuilder> {
    eviction: E,
    hash_builder: S,
    capacity: Option<usize>,
    _marker: PhantomData<(Ev, Eq)>,
}

impl<E: Default, Ev, Eq, S: Default> Default for LocalCacheBuilder<E, Ev, Eq, S> {
    fn default() -> Self {
        Self {
            eviction: Default::default(),
            hash_builder: Default::default(),
            capacity: None,
            _marker: PhantomData,
        }
    }
}

impl<T, E, Ev, Eq, S> BuildCache<T> for LocalCacheBuilder<E, Ev, Eq, S>
where
    T: crate::Value + 'static,
    E: Eviction<Pointer<T, Ev>, Value = Ev, Queue = Eq>,
    S: BuildHasher,
{
    type Cache = LocalCache<T, E, Ev, Eq, S>;

    fn build(mut self) -> Self::Cache {
        let capacity = self.capacity.unwrap_or(16);
        let queue = self.eviction.new_queue(capacity).into();
        let table = RawTable::with_capacity(capacity).into();

        LocalCache {
            table,
            queue,
            hash_builder: self.hash_builder,
            eviction: self.eviction,
        }
    }
}

pub struct LocalCache<T, E, Ev, Eq, S> {
    table: RefCell<RawTable<Pointer<T, Ev>>>,
    queue: RefCell<Eq>,
    hash_builder: S,
    eviction: E,
}

struct Value<T, Ev> {
    inner: T,
    eviction: Ev,
}

pub struct Pointer<T, Ev>(Rc<Value<T, Ev>>);

impl<T, E> Clone for Pointer<T, E> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T, E> Deref for Pointer<T, E> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0.inner
    }
}

impl<T, E, Ev, Eq, S> crate::Cache<T> for LocalCache<T, E, Ev, Eq, S>
where
    T: crate::Value + 'static,
    T::Key: Hash + std::cmp::Eq,
    E: Eviction<Pointer<T, Ev>, Value = Ev, Queue = Eq>,
    S: BuildHasher,
{
    type Pointer = Pointer<T, Ev>;

    const PREFER_LOCKED: bool = true;

    fn len(&self) -> usize {
        self.table.borrow().len()
    }

    fn locked_entry<'c, 'k, K>(
        &'c self,
        key: &'k K,
    ) -> crate::LockedEntry<
        impl crate::LockedOccupiedEntry<Pointer = Self::Pointer> + 'c,
        impl crate::LockedVacantEntry<Pointer = Self::Pointer> + 'c,
    >
    where
        T::Key: Borrow<K>,
        K: ?Sized + std::cmp::Eq + Hash,
    {
        let hash = self.hash_builder.hash_one(key);
        let found = self.table.borrow_mut().find_or_find_insert_slot(
            hash,
            |p| p.0.inner.key().borrow() == key,
            |p| self.hash_builder.hash_one(p.key()),
        );

        match found {
            Ok(bucket) => crate::LockedEntry::Occupied(OccupiedEntry(Some(OccupiedEntryInner {
                cache: self,
                bucket,
            }))),
            Err(slot) => crate::LockedEntry::Vacant(VacantEntry {
                cache: self,
                slot,
                hash,
            }),
        }
    }

    fn entry<'c, 'k, K>(&'c self, key: &'k K) -> impl crate::Entry<Pointer = Self::Pointer> + 'c
        where
            <T as crate::Value>::Key: Borrow<K>,
            K: ?Sized + Hash + std::cmp::Eq + ToOwned<Owned = <T as crate::Value>::Key> 
    {
        
    }
}

struct OccupiedEntry<'a, T: crate::Value, E, Ev, Eq, S>(
    Option<OccupiedEntryInner<'a, T, E, Ev, Eq, S>>,
)
where
    T: crate::Value + 'static,
    E: Eviction<Pointer<T, Ev>, Value = Ev, Queue = Eq>;

struct OccupiedEntryInner<'a, T: crate::Value, E, Ev, Eq, S> {
    cache: &'a LocalCache<T, E, Ev, Eq, S>,
    bucket: Bucket<Pointer<T, Ev>>,
}

impl<T, E, Ev, Eq, S> Drop for OccupiedEntry<'_, T, E, Ev, Eq, S>
where
    T: crate::Value + 'static,
    E: Eviction<Pointer<T, Ev>, Value = Ev, Queue = Eq>,
{
    fn drop(&mut self) {
        if let Some(inner) = self.0.take() {
            // XX Safety
            let pointer = unsafe { inner.bucket.as_ref() };
            inner.cache.eviction.touch(
                &mut *inner.cache.queue.borrow_mut(),
                &pointer.0.eviction,
                &pointer,
            );
        }
    }
}

impl<T: crate::Value, E, Ev, Es, S> OccupiedEntryInner<'_, T, E, Ev, Es, S> {
    fn pointer(&self) -> &Pointer<T, Ev> {
        // XX Safety
        unsafe { self.bucket.as_ref() }
    }
}

impl<T, E, Ev, Es, S> crate::LockedOccupiedEntry for OccupiedEntry<'_, T, E, Ev, Es, S>
where
    T: crate::Value + 'static,
    E: Eviction<Pointer<T, Ev>, Value = Ev, Queue = Es>,
    S: BuildHasher,
{
    type Pointer = Pointer<T, Ev>;

    fn pointer(&self) -> Pointer<T, Ev> {
        self.0.as_ref().unwrap().pointer().clone()
    }

    fn value(&self) -> &T {
        &self.0.as_ref().unwrap().pointer()
    }

    fn replace(mut self, value: T) -> Pointer<T, Ev> {
        let inner = self.0.take().unwrap();

        // XX Safety
        let pointer = unsafe { inner.bucket.as_mut() };

        debug_assert!(value.key() == pointer.key());

        let (replace, evict) = {
            let mut queue = inner.cache.queue.borrow_mut();
            let (replace, evict) = inner.cache.eviction.replace(
                &mut queue,
                &pointer.0.eviction,
                |eviction| {
                    Pointer(Rc::new(Value {
                        inner: value,
                        eviction,
                    }))
                },
            );
            let evict = evict.collect::<SmallVec<[_; 8]>>();
            (replace, evict)
        };
        *pointer = replace.clone();

        for evicted in evict {
            let key = evicted.key();
            let hash = inner.cache.hash_builder.hash_one(key);
            inner.cache
                .table
                .borrow_mut()
                .remove_entry(hash, |p| Rc::ptr_eq(&p.0, &evicted.0));
        }

        replace
    }

    fn remove(mut self) -> Pointer<T, Ev> {
        let inner = self.0.take().unwrap();

        // XX Safety
        let (removed, _slot) = unsafe { inner.cache.table.borrow_mut().remove(inner.bucket) };
        inner
            .cache
            .eviction
            .remove(&mut inner.cache.queue.borrow_mut(), &removed.0.eviction);
        removed
    }
}

struct VacantEntry<'a, T, E, Ev, Eq, S> {
    cache: &'a LocalCache<T, E, Ev, Eq, S>,
    slot: InsertSlot,
    hash: u64,
}

impl<T, E, Ev, Eq, S> crate::LockedVacantEntry for VacantEntry<'_, T, E, Ev, Eq, S>
where
    T: crate::Value + 'static,
    E: Eviction<Pointer<T, Ev>, Value = Ev, Queue = Eq>,
    S: BuildHasher,
{
    type Pointer = Pointer<T, Ev>;

    fn insert(self, value: T) -> Pointer<T, Ev> {
        debug_assert_eq!(self.hash, self.cache.hash_builder.hash_one(value.key()));

        let (insert, evict) = {
            let mut queue = self.cache.queue.borrow_mut();
            let (insert, evict) = self.cache
                .eviction
                .insert(&mut queue, |eviction| {
                    Pointer(Rc::new(Value {
                        inner: value,
                        eviction,
                    }))
                });
            let evict = evict.collect::<SmallVec<[_; 8]>>();
            (insert, evict)
        };

        for evicted in evict {
            let key = evicted.key();
            let hash = self.cache.hash_builder.hash_one(key);
            self.cache
                .table
                .borrow_mut()
                .remove_entry(hash, |p| Rc::ptr_eq(&p.0, &evicted.0));
        }

        // XX: Safety
        unsafe {
            self.cache
                .table
                .borrow_mut()
                .insert_in_slot(self.hash, self.slot, insert.clone());
        }

        insert
    }
}
