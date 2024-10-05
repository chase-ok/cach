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
use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::{Cache, evict::{Eviction, NoEviction}, Value};

pub struct ShardedCache<T, E: Eviction<Entry<T, E>> = NoEviction, S = DefaultHashBuilder> {
    shards: Vec<CachePadded<RwLock<Shard<T, E>>>>,
    hash_builder: S,
    mask: usize,
    eviction: E,
}

struct Shard<T, E: Eviction<Entry<T, E>>> {
    values: RawTable<Arc<InnerEntry<T, E>>>,
    eviction: E::Shard,
}

struct InnerEntry<T, E: Eviction<Entry<T, E>>> {
    value: T,
    eviction: E::Value,
}

pub struct Entry<T, E: Eviction<Entry<T, E>>>(Arc<InnerEntry<T, E>>);

impl<T, E: Eviction<Entry<T, E>>> Clone for Entry<T, E> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T, E: Eviction<Entry<T, E>>> Deref for Entry<T, E> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0.value
    }
}

impl<T, E: Eviction<Entry<T, E>>> InnerEntry<T, E> {
    fn externalize(self: &Arc<Self>) -> Entry<T, E> {
        Entry(Arc::clone(self))
    }
}

impl<T: Value + 'static, E: Eviction<Entry<T, E>>, S: BuildHasher> Cache for ShardedCache<T, E, S>
where
    T::Key: Hash + Eq,
{
    type Value = T;
    type Shared = Entry<T, E>;

    fn len(&self) -> usize {
        self.shards
            .iter()
            .map(|shard| shard.read().values.len())
            .sum()
    }

    fn get<K>(&self, key: &K) -> Option<Self::Shared>
    where
        <Self::Value as Value>::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        let (hash, shard) = self.hash_and_shard(key);
        let shard = self.shards[shard].read();
        let entry = shard
            .values
            .get(hash, |entry| entry.value.key().borrow() == key)
            .map(InnerEntry::externalize)?;

        self.eviction.touch(&shard.eviction, &entry.0.eviction);

        Some(entry)
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

impl<T, E: Eviction<Entry<T, E>>, S: BuildHasher> ShardedCache<T, E, S> {
    fn hash_and_shard(&self, key: &(impl Hash + ?Sized)) -> (u64, usize) {
        let hash = self.hash_builder.hash_one(key);
        // XX is the double hash actually helping?
        let shard = (self.hash_builder.hash_one(hash) as usize) & self.mask;
        (hash, shard)
    }
}

struct OccupiedEntry<'a, T: Value, E: Eviction<Entry<T, E>>, S> {
    cache: &'a ShardedCache<T, E, S>,
    shard: RwLockWriteGuard<'a, Shard<T, E>>,
    bucket: Bucket<Arc<InnerEntry<T, E>>>,
}

struct VacantEntry<'a, T: Value, E: Eviction<Entry<T, E>>, S> {
    cache: &'a ShardedCache<T, E, S>,
    shard: RwLockWriteGuard<'a, Shard<T, E>>,
    slot: InsertSlot,
    hash: u64,
}

impl<T: Value + 'static, E: Eviction<Entry<T, E>>, S: BuildHasher> crate::OccupiedEntry
    for OccupiedEntry<'_, T, E, S>
{
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

    fn replace(mut self, value: <Self::Cache as Cache>::Value) -> <Self::Cache as Cache>::Shared {
        // XX Safety
        let entry = unsafe { self.bucket.as_mut() };

        debug_assert!(value.key() == entry.value.key());

        let replace =
            self.cache
                .eviction
                .replace(&mut self.shard.eviction, &entry.eviction, |eviction| {
                    Entry(Arc::new(InnerEntry { value, eviction }))
                });
        *entry = Arc::clone(&replace.0);

        replace
    }

    fn remove(mut self) -> <Self::Cache as Cache>::Shared {
        // XX Safety
        let (removed, _slot) = unsafe { self.shard.values.remove(self.bucket) };
        self.cache
            .eviction
            .remove(&mut self.shard.eviction, &removed.eviction);
        Entry(removed)
    }
}

impl<T: Value + 'static, E: Eviction<Entry<T, E>>, S: BuildHasher> crate::VacantEntry
    for VacantEntry<'_, T, E, S>
{
    type Cache = ShardedCache<T, E, S>;

    fn insert(mut self, value: <Self::Cache as Cache>::Value) -> <Self::Cache as Cache>::Shared {
        debug_assert_eq!(self.hash, self.cache.hash_builder.hash_one(value.key()));

        let (insert, evict) = self
            .cache
            .eviction
            .insert(&mut self.shard.eviction, |eviction| {
                Entry(Arc::new(InnerEntry { value, eviction }))
            });

        if let Some(Entry(evicted)) = evict {
            let key = evicted.value.key();
            let hash = self.cache.hash_builder.hash_one(key);
            self.shard
                .values
                .remove_entry(hash, |entry| Arc::ptr_eq(entry, &evicted));
        }

        // XX: Safety
        unsafe {
            self.shard
                .values
                .insert_in_slot(self.hash, self.slot, Arc::clone(&insert.0));
        }

        insert
    }
}
