use std::{
    borrow::Borrow,
    hash::{BuildHasher, Hash},
    marker::PhantomData,
    ops::Deref,
    sync::Arc,
    usize,
};

use crossbeam_utils::CachePadded;
use hashbrown::{
    hash_map::DefaultHashBuilder,
    raw::{Bucket, InsertSlot, RawTable},
};
use parking_lot::{RwLock, RwLockWriteGuard};
use smallvec::SmallVec;

use crate::{
    build::BuildCache,
    evict::{Eviction, NoEviction},
    lock::MapUpgradeReadGuard,
    Cache, Mutate, Mutated, 
};

pub const MAX_SHARDS: usize = 2048;

#[derive(Debug, Clone)]
pub struct SyncCacheBuilder<E = NoEviction, Ev = (), Eq = (), S = DefaultHashBuilder> {
    eviction: E,
    hash_builder: S,
    shards: usize,
    capacity: Option<usize>,
    _marker: PhantomData<(Ev, Eq)>,
}

impl<E: Default, Ev, Eq, S: Default> Default for SyncCacheBuilder<E, Ev, Eq, S> {
    fn default() -> Self {
        let target = std::thread::available_parallelism()
            .map(|p| p.get() * 4)
            .unwrap_or(16);
        let shards = target_shards_to_exact(target);

        Self {
            eviction: Default::default(),
            hash_builder: Default::default(),
            shards,
            capacity: None,
            _marker: PhantomData,
        }
    }
}

impl SyncCacheBuilder {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<E, Ev, Eq, S> SyncCacheBuilder<E, Ev, Eq, S> {
    pub fn evict<E2, Ev2, Eq2>(self, eviction: E2) -> SyncCacheBuilder<E2, Ev2, Eq2, S> {
        SyncCacheBuilder {
            eviction,
            hash_builder: self.hash_builder,
            shards: self.shards,
            capacity: self.capacity,
            _marker: PhantomData,
        }
    }

    pub fn hasher<S2>(self, hasher: S2) -> SyncCacheBuilder<E, Ev, Eq, S2> {
        SyncCacheBuilder {
            eviction: self.eviction,
            hash_builder: hasher,
            shards: self.shards,
            capacity: self.capacity,
            _marker: PhantomData,
        }
    }

    pub fn shards(self, shards: usize) -> Self {
        self.exact_shards(target_shards_to_exact(shards))
    }

    pub fn exact_shards(self, shards: usize) -> Self {
        assert!((1..=MAX_SHARDS).contains(&shards));
        assert!(shards.is_power_of_two());
        Self { shards, ..self }
    }

    pub fn capacity(self, capacity: usize) -> Self {
        Self {
            capacity: Some(capacity),
            ..self
        }
    }
}

impl<T, E, Ev, Eq, S> BuildCache<T> for SyncCacheBuilder<E, Ev, Eq, S>
where
    T: crate::Value + 'static,
    E: Eviction<Pointer<T, Ev>, Value = Ev, Queue = Eq>,
    S: BuildHasher,
{
    type Cache = SyncCache<T, E, Ev, Eq, S>;

    fn build(mut self) -> Self::Cache {
        let capacity = self
            .capacity
            .unwrap_or_else(|| self.shards.saturating_mul(16));
        let capacity_per_shard = self.shards.div_ceil(capacity);

        let shards = std::iter::repeat_with(|| {
            CachePadded::new(RwLock::new(Shard {
                values: RawTable::with_capacity(capacity_per_shard),
                eviction: self.eviction.new_queue(capacity_per_shard),
            }))
        })
        .take(self.shards)
        .collect();

        SyncCache {
            shards,
            hash_builder: self.hash_builder,
            mask: self.shards - 1,
            eviction: self.eviction,
        }
    }
}

fn target_shards_to_exact(target: usize) -> usize {
    target
        .checked_next_power_of_two()
        .unwrap_or(usize::MAX)
        .min(MAX_SHARDS)
}

pub struct SyncCache<T, E, Ev, Eq, S> {
    shards: Vec<CachePadded<RwLock<Shard<T, Ev, Eq>>>>,
    hash_builder: S,
    mask: usize,
    eviction: E,
}

struct Shard<T, Ev, Eq> {
    values: RawTable<Pointer<T, Ev>>,
    eviction: Eq,
}

struct Value<T, E> {
    value: T,
    eviction: E,
}

pub struct Pointer<T, E>(Arc<Value<T, E>>);

impl<T, E> Clone for Pointer<T, E> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T, E> Deref for Pointer<T, E> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0.value
    }
}

impl<T, E, Ev, Eq, S> Cache<T> for SyncCache<T, E, Ev, Eq, S>
where
    T: crate::Value + 'static,
    T::Key: Hash + std::cmp::Eq,
    E: Eviction<Pointer<T, Ev>, Value = Ev, Queue = Eq>,
    S: BuildHasher,
{
    type Pointer = Pointer<T, Ev>;

    const PREFER_LOCKED: bool = true;

    fn len(&self) -> usize {
        self.shards
            .iter()
            .map(|shard| shard.read().values.len())
            .sum()
    }

    fn get<K>(&self, key: &K) -> Option<Self::Pointer>
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + std::cmp::Eq,
    {
        let (hash, shard) = self.hash_and_shard(key);
        let shard = self.shards[shard].read();
        let pointer = shard
            .values
            .get(hash, |p| p.0.value.key().borrow() == key)?
            .clone();

        let touch_guard = MapUpgradeReadGuard::new(shard, |s| &s.eviction, |s| &mut s.eviction);
        self.eviction
            .touch(touch_guard, &pointer.0.eviction, &pointer);

        Some(pointer)
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
        let (hash, shard) = self.hash_and_shard(key);

        let mut shard = self.shards[shard].write();
        let found = shard.values.find_or_find_insert_slot(
            hash,
            |p| p.0.value.key().borrow() == key,
            |p| self.hash_builder.hash_one(p.key()),
        );
        match found {
            Ok(bucket) => crate::LockedEntry::Occupied(OccupiedEntry(Some(OccupiedEntryInner {
                cache: self,
                shard,
                bucket,
            }))),
            Err(slot) => crate::LockedEntry::Vacant(VacantEntry {
                cache: self,
                shard,
                slot,
                hash,
            }),
        }
    }

    fn entry<'c, 'k, K>(&'c self, key: &'k K) -> impl crate::Entry<Pointer = Self::Pointer> + 'c
    where
        <T as crate::Value>::Key: Borrow<K>,
        K: ?Sized + Hash + std::cmp::Eq + ToOwned<Owned = <T as crate::Value>::Key>,
    {
        let (hash, shard) = self.hash_and_shard(key);
        let current = self.shards[shard]
            .read()
            .values
            .get(hash, |p| p.0.value.key().borrow() == key)
            .cloned();
        Entry {
            cache: self,
            current: match current {
                Some(o) => EntryState::Occupied(o),
                None => EntryState::Vacant(key.to_owned()),
            },
            shard,
            hash,
        }
    }
}

impl<T, E, Ev, Eq, S: BuildHasher> SyncCache<T, E, Ev, Eq, S> {
    fn hash_and_shard(&self, key: &(impl Hash + ?Sized)) -> (u64, usize) {
        let hash = self.hash_builder.hash_one(key);
        // XX is the double hash actually helping?
        let shard = (self.hash_builder.hash_one(hash) as usize) & self.mask;
        (hash, shard)
    }
}

struct OccupiedEntry<'a, T: crate::Value, E, Ev, Eq, S>(
    Option<OccupiedEntryInner<'a, T, E, Ev, Eq, S>>,
)
where
    T: crate::Value + 'static,
    E: Eviction<Pointer<T, Ev>, Value = Ev, Queue = Eq>;

struct OccupiedEntryInner<'a, T: crate::Value, E, Ev, Eq, S> {
    cache: &'a SyncCache<T, E, Ev, Eq, S>,
    shard: RwLockWriteGuard<'a, Shard<T, Ev, Eq>>,
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
            let touch_guard =
                MapUpgradeReadGuard::new(inner.shard, |s| &s.eviction, |s| &mut s.eviction);
            inner
                .cache
                .eviction
                .touch(touch_guard, &pointer.0.eviction, &pointer);
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
        let mut this = self.0.take().unwrap();

        // XX Safety
        let pointer = unsafe { this.bucket.as_mut() };

        debug_assert!(value.key() == pointer.key());

        let (replace, evict) = {
            let (replace, evict) = this.cache.eviction.replace(
                &mut this.shard.eviction,
                &pointer.0.eviction,
                |eviction| Pointer(Arc::new(Value { value, eviction })),
            );
            let evict = evict.collect::<SmallVec<[_; 8]>>();
            (replace, evict)
        };
        *pointer = replace.clone();

        for evicted in evict {
            this.shard
                .values
                .remove_entry(this.cache.hash_builder.hash_one(evicted.key()), |p| {
                    Arc::ptr_eq(&p.0, &evicted.0)
                });
        }

        replace
    }

    fn remove(mut self) -> Pointer<T, Ev> {
        let mut inner = self.0.take().unwrap();

        // XX Safety
        let (removed, _slot) = unsafe { inner.shard.values.remove(inner.bucket) };
        inner
            .cache
            .eviction
            .remove(&mut inner.shard.eviction, &removed.0.eviction);
        removed
    }
}

struct VacantEntry<'a, T, E, Ev, Eq, S> {
    cache: &'a SyncCache<T, E, Ev, Eq, S>,
    shard: RwLockWriteGuard<'a, Shard<T, Ev, Eq>>,
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

    fn insert(mut self, value: T) -> Pointer<T, Ev> {
        debug_assert_eq!(self.hash, self.cache.hash_builder.hash_one(value.key()));

        let (insert, evict) = {
            let (insert, evict) = self
                .cache
                .eviction
                .insert(&mut self.shard.eviction, |eviction| {
                    Pointer(Arc::new(Value { value, eviction }))
                });
            let evict = evict.collect::<SmallVec<[_; 8]>>();
            (insert, evict)
        };

        for evicted in evict {
            let key = evicted.key();
            let hash = self.cache.hash_builder.hash_one(key);
            self.shard
                .values
                .remove_entry(hash, |p| Arc::ptr_eq(&p.0, &evicted.0));
        }

        // XX: Safety
        unsafe {
            self.shard
                .values
                .insert_in_slot(self.hash, self.slot, insert.clone());
        }

        insert
    }
}

struct Entry<'a, T: crate::Value, E, Ev, Eq, S> {
    cache: &'a SyncCache<T, E, Ev, Eq, S>,
    current: EntryState<T, Ev>,
    shard: usize,
    hash: u64,
}

enum EntryState<T: crate::Value, Ev> {
    Occupied(Pointer<T, Ev>),
    Removed(Pointer<T, Ev>),
    Vacant(T::Key),
}

impl<T, E, Ev, Es, S> crate::Entry for Entry<'_, T, E, Ev, Es, S>
where
    T: crate::Value + 'static,
    E: Eviction<Pointer<T, Ev>, Value = Ev, Queue = Es>,
    S: BuildHasher,
{
    type Pointer = Pointer<T, Ev>;

    fn value(&self) -> Option<&T> {
        match &self.current {
            EntryState::Occupied(pointer) => Some(&**pointer),
            _ => None,
        }
    }

    fn pointer(&self) -> Option<Self::Pointer> {
        match &self.current {
            EntryState::Occupied(pointer) => Some(pointer.clone()),
            _ => None,
        }
    }

    fn key(&self) -> &T::Key {
        match &self.current {
            EntryState::Occupied(pointer) | EntryState::Removed(pointer) => pointer.key(),
            EntryState::Vacant(key) => &key,
        }
    }

    fn try_mutate(&mut self, mutate: Mutate<T>) -> Result<Mutated<Self::Pointer>, Mutate<T>> {
        match (&self.current, mutate) {
            // Occupied, but unchanged?
            (EntryState::Occupied(current), Mutate::None) => {
                let next = self.cache.shards[self.shard]
                    .read()
                    .values
                    .get(self.hash, |p| Arc::ptr_eq(&p.0, &current.0) || p.key() == current.key())
                    .cloned();

                match next {
                    Some(next) if Arc::ptr_eq(&current.0, &next.0) => Ok(Mutated::None(Some(next))),
                    Some(next) => {
                        self.current = EntryState::Occupied(next);
                        Err(Mutate::None)
                    }
                    None => {
                        // XX: make swap method that doesn't require clone
                        self.current = EntryState::Removed(current.clone());
                        Err(Mutate::None)
                    }
                }
            }

            // Removed, but unchanged?
            (EntryState::Removed(current), Mutate::None) => {
                let next = self.cache.shards[self.shard]
                    .read()
                    .values
                    .get(self.hash, |p| Arc::ptr_eq(&p.0, &current.0) || p.key() == current.key())
                    .cloned();

                match next {
                    Some(next) => {
                        self.current = EntryState::Occupied(next);
                        Err(Mutate::None)
                    }
                    None => {
                        Ok(Mutated::None(None))
                    }
                }
            }

            // Vacant, but unchanged?
            (EntryState::Vacant(key), Mutate::None) => {
                let next = self.cache.shards[self.shard]
                    .read()
                    .values
                    .get(self.hash, |p| p.key() == key)
                    .cloned();

                match next {
                    Some(next) => {
                        self.current = EntryState::Occupied(next);
                        Err(Mutate::None)
                    }
                    None => {
                        Ok(Mutated::None(None))
                    }
                }
            }

            // Write to occupied
            (EntryState::Occupied(current), mutate @ (Mutate::Write(_) | Mutate::Remove)) => {
                let mut shard = self.cache.shards[self.shard].write();
                let found = shard.values.find(
                    self.hash, 
                    |p| Arc::ptr_eq(&p.0, &current.0) || p.key() == current.key(),
                );

                if let Some(bucket) = found {
                    // XX safety
                    let next = unsafe { bucket.as_mut() };
                    if Arc::ptr_eq(&next.0, &current.0) {
                        match mutate {
                            Mutate::Write(value) => {
                                // let replaced = std::mem::replace(next, );
                                todo!()
                            }
                            Mutate::Remove => {
                                drop(next);
                                // XX safety
                                unsafe { shard.values.remove(bucket); }
                                let removed = current.clone();
                                self.current = EntryState::Removed(current.clone());
                                Ok(Mutated::Removed(removed))
                            }
                            _ => unreachable!()
                        }
                    } else {
                        self.current = EntryState::Occupied(next.clone());
                        Err(mutate)
                    }
                } else {
                    self.current = EntryState::Removed(current.clone());
                    Err(mutate)
                }
            }

            _ => todo!()
            // (Err(_), Mutate::Write(_)) => todo!(),
            // (Err(_), Mutate::Remove) => todo!(),
        }
    }
}
