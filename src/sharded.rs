use std::{
    borrow::Borrow,
    hash::{BuildHasher, Hash},
    ops::Deref,
    sync::Arc,
};

use crossbeam_utils::CachePadded;
use hashbrown::{
    hash_map::DefaultHashBuilder,
    raw::{Bucket, InsertSlot, RawTable},
};
use parking_lot::{RwLock, RwLockWriteGuard};

use crate::{Cache, EvictionStrategy, NoEviction, Value};

pub struct ShardedCache<T, E: EvictionStrategy<ShardedEntry<T, E>> = NoEviction, S = DefaultHashBuilder> {
    shards: Vec<CachePadded<RwLock<Shard<T, E>>>>,
    hash_builder: S,
    mask: usize,
    eviction: E,
}

struct Shard<T, E: EvictionStrategy<ShardedEntry<T, E>>> {
    values: RawTable<Arc<InnerEntry<T, E>>>,
    eviction_state: E::ShardState,
}

struct InnerEntry<T, E: EvictionStrategy<ShardedEntry<T, E>>> {
    value: T,
    eviction_state: E::ValueState,
}

pub struct ShardedEntry<T, E: EvictionStrategy<ShardedEntry<T, E>>>(Arc<InnerEntry<T, E>>);

impl<T, E: EvictionStrategy<ShardedEntry<T, E>>> Clone for ShardedEntry<T, E> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T, E: EvictionStrategy<ShardedEntry<T, E>>> Deref for ShardedEntry<T, E> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0.value
    }
}

impl<T, E: EvictionStrategy<ShardedEntry<T, E>>> InnerEntry<T, E> {
    fn externalize(self: &Arc<Self>) -> ShardedEntry<T, E> {
        ShardedEntry(Arc::clone(self))
    }
}

impl<T: Value + 'static, E: EvictionStrategy<ShardedEntry<T, E>>, S: BuildHasher> Cache for ShardedCache<T, E, S>
where
    T::Key: Hash + Eq,
{
    type Value = T;
    type Shared = ShardedEntry<T, E>;

    fn len(&self) -> usize {
        self.shards.iter().map(|shard| shard.read().values.len()).sum()
    }

    fn get<K>(&self, key: &K) -> Option<Self::Shared>
    where
        <Self::Value as Value>::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        let (hash, shard) = self.hash_and_shard(key);
        let shard = self.shards[shard].read();
        shard
            .get(hash, |entry| entry.value.key().borrow() == key)
            .map(InnerEntry::externalize)
    }

    fn entry<'c, 'k, K>(
        &'c self,
        key: &'k K,
    ) -> crate::Entry<
        impl crate::OccupiedEntry<Cache = Self> + 'c,
        impl crate::VacantEntry<Cache = Self> + 'c,
    >
    where
        <Self::Value as Value>::Key: Borrow<K>,
        K: ?Sized + Eq + Hash,
    {
        let (hash, shard) = self.hash_and_shard(key);

        let mut shard = self.shards[shard].write();
        let found = shard.values.find_or_find_insert_slot(
            hash,
            |entry| entry.value.key().borrow() == key,
            |entry| self.hash_builder.hash_one(entry.value.key()),
        );
        match found {
            Ok(bucket) => crate::Entry::Occupied(OccupiedEntry {
                cache: self,
                shard,
                bucket,
            }),
            Err(slot) => crate::Entry::Vacant(VacantEntry {
                cache: self,
                shard,
                slot,
                hash,
            }),
        }
    }
}

impl<T, E: EvictionStrategy<ShardedEntry<T, E>>, S: BuildHasher> ShardedCache<T, E, S> {
    fn hash_and_shard(&self, key: &(impl Hash + ?Sized)) -> (u64, usize) {
        let hash = self.hash_builder.hash_one(key);
        // XX is the double hash actually helping?
        let shard = (self.hash_builder.hash_one(key) as usize) & self.mask;
        (hash, shard)
    }
}

struct OccupiedEntry<'a, T: Value, E: EvictionStrategy<ShardedEntry<T, E>>, S> {
    cache: &'a ShardedCache<T, E, S>,
    shard: RwLockWriteGuard<'a, Shard<T, E>>,
    bucket: Bucket<Arc<InnerEntry<T, E>>>,
}

struct VacantEntry<'a, T: Value, E: EvictionStrategy<ShardedEntry<T, E>>, S> {
    cache: &'a ShardedCache<T, E, S>,
    shard: RwLockWriteGuard<'a, Shard<T, E>>,
    slot: InsertSlot,
    hash: u64,
}

impl<T: Value + 'static, E: EvictionStrategy<ShardedEntry<T, E>>, S: BuildHasher> crate::OccupiedEntry for OccupiedEntry<'_, T, E, S> {
    type Cache = ShardedCache<T, E, S>;

    fn shared(&self) -> <Self::Cache as Cache>::Shared {
        // XX Safety
        let entry = unsafe { self.bucket.as_ref() };
        entry.externalize()
    }

    fn value(&self) -> &<Self::Cache as Cache>::Value {
        // XX Safety
        let entry = unsafe { self.bucket.as_ref() };
        &entry.value
    }

    fn replace(self, value: <Self::Cache as Cache>::Value) -> <Self::Cache as Cache>::Shared {
        // XX Safety
        let entry = unsafe { self.bucket.as_mut() };
        *entry = Arc::new(InnerEntry { value, eviction_state: self.cache.eviction.new_value(&mut self.shard.eviction_state, value_ref) });
        entry.externalize()
    }

    fn remove(mut self) -> <Self::Cache as Cache>::Shared {
        // XX Safety
        let (removed, _slot) = unsafe { self.shard.values.remove(self.bucket) };
        self.cache.eviction.remove_value(&mut self.shard.eviction_state, &removed.eviction_state);
        ShardedEntry(removed)
    }
}

impl<T: Value + 'static, E: EvictionStrategy<ShardedEntry<T, E>>, S: BuildHasher> crate::VacantEntry for VacantEntry<'_, T, E, S> {
    type Cache = ShardedCache<T, E, S>;

    fn insert(mut self, value: <Self::Cache as Cache>::Value) -> <Self::Cache as Cache>::Shared {
        let entry = Arc::new(InnerEntry { value });
        // XX: Safety
        unsafe {
            self.shard
                .values
                .insert_in_slot(self.hash, self.slot, Arc::clone(&entry));
        }
        ShardedEntry(entry)
    }
}
