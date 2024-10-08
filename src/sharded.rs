use std::{
    borrow::Borrow,
    hash::{BuildHasher, Hash},
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

use crate::{
    evict::{BuildEviction, Eviction, MapUpgradeReadGuard, NoEviction},
    BuildCache, Cache,
};

pub const MAX_SHARDS: usize = 2048;

#[derive(Debug, Clone)]
pub struct ShardedCacheBuilder<E = NoEviction, S = DefaultHashBuilder> {
    eviction: E,
    hash_builder: S,
    shards: usize,
    capacity: Option<usize>,
}

impl<E: Default, S: Default> Default for ShardedCacheBuilder<E, S> {
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
        }
    }
}

impl ShardedCacheBuilder {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<E, S> ShardedCacheBuilder<E, S> {
    pub fn eviction<E2>(self, eviction: E2) -> ShardedCacheBuilder<E2, S> {
        ShardedCacheBuilder {
            eviction,
            hash_builder: self.hash_builder,
            shards: self.shards,
            capacity: self.capacity,
        }
    }

    pub fn hasher<S2>(self, hasher: S2) -> ShardedCacheBuilder<E, S2> {
        ShardedCacheBuilder {
            eviction: self.eviction,
            hash_builder: hasher,
            shards: self.shards,
            capacity: self.capacity,
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
        Self { capacity: Some(capacity), ..self }
    }
}

fn target_shards_to_exact(target: usize) -> usize {
    target
        .checked_next_power_of_two()
        .unwrap_or(usize::MAX)
        .min(MAX_SHARDS)
}

impl<T: crate::Value, E: BuildEviction, S: BuildHasher> BuildCache<T> for ShardedCacheBuilder<E, S> {
    fn build(self) -> impl Cache<T> {
        let mut eviction = self.eviction.build();

        let capacity = self
            .capacity
            .unwrap_or_else(|| self.shards.saturating_mul(16));
        let capacity_per_shard = self.shards.div_ceil(capacity);

        let shards = std::iter::repeat_with(|| {
            CachePadded::new(RwLock::new(Shard {
                values: RawTable::with_capacity(capacity_per_shard),
                eviction: eviction.new_shard(capacity_per_shard),
            }))
        })
        .take(self.shards)
        .collect();

        ShardedCache {
            shards,
            hash_builder: self.hash_builder,
            mask: self.shards - 1,
            eviction,
        }
    }
}

struct ShardedCache<T, E: Eviction<Pointer<T, E>> = NoEviction, S = DefaultHashBuilder> {
    shards: Vec<CachePadded<RwLock<Shard<T, E>>>>,
    hash_builder: S,
    mask: usize,
    eviction: E,
}

struct Shard<T, E: Eviction<Pointer<T, E>>> {
    values: RawTable<Pointer<T, E>>,
    eviction: E::Shard,
}

struct Value<T, E: Eviction<Pointer<T, E>>> {
    value: T,
    eviction: E::State,
}

pub struct Pointer<T, E: Eviction<Pointer<T, E>>>(Arc<Value<T, E>>);

impl<T, E: Eviction<Pointer<T, E>>> Clone for Pointer<T, E> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T, E: Eviction<Pointer<T, E>>> Deref for Pointer<T, E> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0.value
    }
}

impl<T: crate::Value + 'static, E: Eviction<Pointer<T, E>>, S: BuildHasher> Cache<T>
    for ShardedCache<T, E, S>
where
    T::Key: Hash + Eq,
{
    type Pointer = Pointer<T, E>;

    fn len(&self) -> usize {
        self.shards
            .iter()
            .map(|shard| shard.read().values.len())
            .sum()
    }

    fn get<K>(&self, key: &K) -> Option<Self::Pointer>
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
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

    fn entry<'c, 'k, K>(
        &'c self,
        key: &'k K,
    ) -> crate::Entry<
        impl crate::OccupiedEntry<Pointer = Self::Pointer> + 'c,
        impl crate::VacantEntry<Pointer = Self::Pointer> + 'c,
    >
    where
        T::Key: Borrow<K>,
        K: ?Sized + Eq + Hash,
    {
        let (hash, shard) = self.hash_and_shard(key);

        let mut shard = self.shards[shard].write();
        let found = shard.values.find_or_find_insert_slot(
            hash,
            |p| p.0.value.key().borrow() == key,
            |p| self.hash_builder.hash_one(p.key()),
        );
        match found {
            Ok(bucket) => crate::Entry::Occupied(OccupiedEntry(Some(OccupiedEntryInner {
                cache: self,
                shard,
                bucket,
            }))),
            Err(slot) => crate::Entry::Vacant(VacantEntry {
                cache: self,
                shard,
                slot,
                hash,
            }),
        }
    }
}

impl<T, E: Eviction<Pointer<T, E>>, S: BuildHasher> ShardedCache<T, E, S> {
    fn hash_and_shard(&self, key: &(impl Hash + ?Sized)) -> (u64, usize) {
        let hash = self.hash_builder.hash_one(key);
        // XX is the double hash actually helping?
        let shard = (self.hash_builder.hash_one(hash) as usize) & self.mask;
        (hash, shard)
    }
}

struct OccupiedEntry<'a, T: crate::Value, E: Eviction<Pointer<T, E>>, S>(
    Option<OccupiedEntryInner<'a, T, E, S>>,
);

struct OccupiedEntryInner<'a, T: crate::Value, E: Eviction<Pointer<T, E>>, S> {
    cache: &'a ShardedCache<T, E, S>,
    shard: RwLockWriteGuard<'a, Shard<T, E>>,
    bucket: Bucket<Pointer<T, E>>,
}

impl<T: crate::Value, E: Eviction<Pointer<T, E>>, S> Drop for OccupiedEntry<'_, T, E, S> {
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

impl<T: crate::Value, E: Eviction<Pointer<T, E>>, S> OccupiedEntryInner<'_, T, E, S> {
    fn pointer(&self) -> &Pointer<T, E> {
        // XX Safety
        unsafe { self.bucket.as_ref() }
    }
}

struct VacantEntry<'a, T: crate::Value, E: Eviction<Pointer<T, E>>, S> {
    cache: &'a ShardedCache<T, E, S>,
    shard: RwLockWriteGuard<'a, Shard<T, E>>,
    slot: InsertSlot,
    hash: u64,
}

impl<T: crate::Value + 'static, E: Eviction<Pointer<T, E>>, S: BuildHasher> crate::OccupiedEntry
    for OccupiedEntry<'_, T, E, S>
{
    type Pointer = Pointer<T, E>;

    fn pointer(&self) -> Pointer<T, E> {
        self.0.as_ref().unwrap().pointer().clone()
    }

    fn value(&self) -> &T {
        &self.0.as_ref().unwrap().pointer()
    }

    fn replace(mut self, value: T) -> Pointer<T, E> {
        let mut inner = self.0.take().unwrap();

        // XX Safety
        let pointer = unsafe { inner.bucket.as_mut() };

        debug_assert!(value.key() == pointer.key());

        let replace = inner.cache.eviction.replace(
            &mut inner.shard.eviction,
            &pointer.0.eviction,
            |eviction| Pointer(Arc::new(Value { value, eviction })),
        );
        *pointer = replace.clone();

        replace
    }

    fn remove(mut self) -> Pointer<T, E> {
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

impl<T: crate::Value + 'static, E: Eviction<Pointer<T, E>>, S: BuildHasher> crate::VacantEntry
    for VacantEntry<'_, T, E, S>
{
    type Pointer = Pointer<T, E>;

    fn insert(mut self, value: T) -> Pointer<T, E> {
        debug_assert_eq!(self.hash, self.cache.hash_builder.hash_one(value.key()));

        let (insert, evict) = self
            .cache
            .eviction
            .insert(&mut self.shard.eviction, |eviction| {
                Pointer(Arc::new(Value { value, eviction }))
            });

        if let Some(evicted) = evict {
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
