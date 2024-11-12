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
use stable_deref_trait::{CloneStableDeref, StableDeref};

use crate::{
    layer::{Layer, LayerNone, ReadLockBehavior, ReadResult, Resolve, Shard as ShardLayer},
    lock::{MapUpgradeReadGuard, UpgradeReadGuard, UpgradeReadGuardCell},
    Cache,
};

pub const MAX_SHARDS: usize = 2048;

#[derive(Debug, Clone)]
pub struct SyncCacheBuilder<S = DefaultHashBuilder> {
    hash_builder: S,
    shards: usize,
    capacity: Option<usize>,
}

impl<S: Default> Default for SyncCacheBuilder<S> {
    fn default() -> Self {
        let target = std::thread::available_parallelism()
            .map(|p| p.get() * 4)
            .unwrap_or(16);
        let shards = target_shards_to_exact(target);

        Self {
            hash_builder: Default::default(),
            shards,
            capacity: None,
        }
    }
}

impl SyncCacheBuilder {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<S> SyncCacheBuilder<S> {
    // pub fn evict<E2, Ev2, Eq2>(self, eviction: E2) -> SyncCacheBuilder<E2, Ev2, Eq2, S> {
    //     SyncCacheBuilder {
    //         layer: eviction,
    //         hash_builder: self.hash_builder,
    //         shards: self.shards,
    //         capacity: self.capacity,
    //         _marker: PhantomData,
    //     }
    // }

    pub fn hasher<S2>(self, hasher: S2) -> SyncCacheBuilder<S2> {
        SyncCacheBuilder {
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
        Self {
            capacity: Some(capacity),
            ..self
        }
    }

    pub fn build_with_layer<T, L, Lv, Ls>(self, layer: L) -> SyncCache<T, Lv, Ls, S>
    where 
        T: crate::Value,
        L: Layer<Pointer<T, Lv>, Value = Lv, Shard = Ls>,
    {
        let capacity = self
            .capacity
            .unwrap_or_else(|| self.shards.saturating_mul(16));
        let capacity_per_shard = self.shards.div_ceil(capacity);

        let shards = std::iter::repeat_with(|| {
            CachePadded::new(RwLock::new(Shard {
                values: RawTable::with_capacity(capacity_per_shard),
                layer: layer.new_shard(capacity_per_shard),
            }))
        })
        .take(self.shards)
        .collect();

        SyncCache {
            shards,
            hash_builder: self.hash_builder,
            mask: self.shards - 1,
            capacity_per_shard,
        }
    }
}

// impl<T, L, Lv, Ls, S> BuildCache<T> for SyncCacheBuilder<L, Lv, Ls, S>
// where
//     T: crate::Value + 'static,
//     L: Layer<Pointer<T, Lv>, Value = Lv, Shard = Ls>,
//     S: BuildHasher,
// {
//     type Cache = SyncCache<T, Lv, Ls, S>;

//     fn build(self) -> Self::Cache {
//         let capacity = self
//             .capacity
//             .unwrap_or_else(|| self.shards.saturating_mul(16));
//         let capacity_per_shard = self.shards.div_ceil(capacity);

//         let shards = std::iter::repeat_with(|| {
//             CachePadded::new(RwLock::new(Shard {
//                 values: RawTable::with_capacity(capacity_per_shard),
//                 layer: self.layer.new_shard(capacity_per_shard),
//             }))
//         })
//         .take(self.shards)
//         .collect();

//         SyncCache {
//             shards,
//             hash_builder: self.hash_builder,
//             mask: self.shards - 1,
//             capacity_per_shard,
//         }
//     }
// }

fn target_shards_to_exact(target: usize) -> usize {
    target
        .checked_next_power_of_two()
        .unwrap_or(usize::MAX)
        .min(MAX_SHARDS)
}

// XX: Can remove L!
pub struct SyncCache<T, Lv, Ls, S = DefaultHashBuilder> {
    shards: Vec<CachePadded<RwLock<Shard<T, Lv, Ls>>>>,
    hash_builder: S,
    mask: usize,
    capacity_per_shard: usize,
}



struct Shard<T, Lv, Ls> {
    values: RawTable<Pointer<T, Lv>>,
    layer: Ls,
}

struct Value<T, L> {
    value: T,
    layer: L,
}

pub struct Pointer<T, L>(Arc<Value<T, L>>);

impl<T, L> Clone for Pointer<T, L> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T, L> Deref for Pointer<T, L> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0.value
    }
}

// XX: just a wrapper around Arc<> that does impl Stable/Clone
unsafe impl<T, L> StableDeref for Pointer<T, L> {}
unsafe impl<T, L> CloneStableDeref for Pointer<T, L> {}

struct ResolveLayer;

impl<T, L> Resolve<Pointer<T, L>, L> for ResolveLayer {
    fn resolve(pointer: &Pointer<T, L>) -> &L {
        &pointer.0.layer
    }
}

impl<T, Lv, Ls, S> Cache<T> for SyncCache<T, Lv, Ls, S>
where
    T: crate::Value + 'static,
    T::Key: Hash + std::cmp::Eq,
    Ls: ShardLayer<Pointer<T, Lv>, Value = Lv>,
    S: BuildHasher,
{
    type Pointer = Pointer<T, Lv>;

    fn len(&self) -> usize {
        self.shards
            .iter()
            .map(|shard| shard.read().values.len())
            .sum()
    }

    fn iter(&self) -> impl Iterator<Item = Self::Pointer> {
        self.shards.iter().flat_map(|shard| {
            let mut pointers = Vec::new();
            'attempt: loop {
                pointers.clear();

                // XX
                let buckets_len = {
                    let shard = shard.read();
                    pointers.reserve(shard.values.len());
                    shard.values.buckets()
                };

                const CHUNK: usize = 256;
                let mut i = 0;
                while i < buckets_len {
                    // XX: typical case = 1 iterating thread, ok to use upgradable read since we don't block
                    // need atomic upgrade for efficiently deleting
                    let mut shard = UpgradeReadGuardCell::new(shard.upgradable_read());

                    if shard.values.buckets() != buckets_len {
                        continue 'attempt; // resized in between
                    }

                    for bucket in i..buckets_len.min(i + CHUNK) {
                        // XX safety
                        if unsafe { shard.values.is_bucket_full(bucket) } {
                            // XX safety
                            let bucket = unsafe { shard.values.bucket(bucket) };
                            // XX safety
                            let pointer = unsafe { bucket.as_ref() }.clone();

                            let layer = MapUpgradeReadGuard::new(shard.guard(), |s| &s.layer, |s| &mut s.layer);
                            match Ls::iter_read::<ResolveLayer>(layer, &pointer) {
                                ReadResult::Retain => {
                                    pointers.push(pointer);
                                }
                                ReadResult::Remove => {
                                    // XX atomic safety
                                    let mut shard = shard.guard().atomic_upgrade();
                                    shard.layer.remove::<ResolveLayer>(&pointer);
                                    // XX safety
                                    unsafe {
                                        shard.values.remove(bucket);
                                    }
                                }
                            }
                        }
                    }

                    i += CHUNK
                }
                break;
            }
            pointers
        })
        // struct Iter<I, T, Lv, Ls> {
        //     iter: I,
        //     pointers: Vec<Pointer<T, Lv>>,
        //     _marker: PhantomData<Ls>,
        // }

        // impl<'a, I, T: 'a, Lv: 'a, Ls: 'a> Iterator for Iter<I, T, Lv, Ls>
        // where
        //     I: Iterator<Item = &'a CachePadded<RwLock<Shard<T, Lv, Ls>>>>,
        //     Ls: ShardLayer<Pointer<T, Lv>, Value = Lv>,
        // {
        //     type Item = Pointer<T, Lv>;

        //     fn next(&mut self) -> Option<Self::Item> {
        //         loop {
        //             if let Some(pointer) = self.pointers.pop() {
        //                 return Some(pointer);
        //             } else if let Some(shard) = self.iter.next() {
        //                 match Ls::READ_LOCK_BEHAVIOR {
        //                     ReadLockBehavior::ReadLockOnly => todo!(),
        //                     ReadLockBehavior::RequireWriteLock => {
        //                         let shard = shard.write();
        //                         shard.values.is_bucket_full(index)
        //                     }
        //                 }
        //                 // XX safety
        //                 self.pointers.extend(unsafe {
        //                     shard.read().values.iter().map(|b| b.as_ref().clone())
        //                 });
        //             } else {
        //                 return None;
        //             }
        //         }
        //     }
        // }

        // Iter {
        //     iter: self.shards.iter(),
        //     pointers: Vec::with_capacity(self.capacity_per_shard),
        //     _marker: PhantomData,
        // }
    }

    fn get<K>(&self, key: &K) -> Option<Self::Pointer>
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + std::cmp::Eq,
    {
        let (hash, shard) = self.hash_and_shard(key);
        match Ls::READ_LOCK_BEHAVIOR {
            ReadLockBehavior::ReadLockOnly => {
                let shard = self.shards[shard].read();
                let pointer = shard
                    .values
                    .get(hash, |p| p.0.value.key().borrow() == key)?
                    .clone();

                let mut cell = UpgradeReadGuardCell::new(shard);
                let layer_shard =
                    MapUpgradeReadGuard::new(cell.guard(), |s| &s.layer, |s| &mut s.layer);
                match Ls::read::<ResolveLayer>(layer_shard, &pointer) {
                    ReadResult::Retain => Some(pointer),
                    ReadResult::Remove => {
                        let mut shard = cell.guard().upgrade();
                        // XX: need to re-look up because someone else might've beat us to the write lock
                        let pointer = shard
                            .values
                            .remove_entry(hash, |p| Arc::ptr_eq(&p.0, &pointer.0))?;
                        shard.layer.remove::<ResolveLayer>(&pointer);
                        Some(pointer)
                    }
                }
            }

            ReadLockBehavior::RequireWriteLock => {
                let mut shard = self.shards[shard].write();
                let bucket = shard
                    .values
                    .find(hash, |p| p.0.value.key().borrow() == key)?;
                // XX: safety
                let pointer = unsafe { bucket.as_ref() };
                match Ls::read::<ResolveLayer>(&mut shard.layer, &pointer) {
                    ReadResult::Retain => Some(pointer.clone()),
                    ReadResult::Remove => {
                        shard.layer.remove::<ResolveLayer>(pointer);
                        // XX: safety
                        unsafe {
                            shard.values.remove(bucket);
                        }
                        None
                    }
                }
            }
        }
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
        K: ?Sized + std::cmp::Eq + Hash,
    {
        let (hash, shard_index) = self.hash_and_shard(key);

        let mut shard = self.shards[shard_index].write();
        let found = shard.values.find_or_find_insert_slot(
            hash,
            |p| p.0.value.key().borrow() == key,
            |p| self.hash_builder.hash_one(p.key()),
        );
        match found {
            Ok(bucket) => {
                // XX safety
                let pointer = unsafe { bucket.as_ref() };
                match Ls::read::<ResolveLayer>(&mut shard.layer, pointer) {
                    ReadResult::Retain => crate::Entry::Occupied(OccupiedEntry {
                        cache: self,
                        shard,
                        bucket,
                        shard_index,
                    }),
                    ReadResult::Remove => {
                        shard.layer.remove::<ResolveLayer>(pointer);
                        // XX safety
                        let (_pointer, slot) = unsafe { shard.values.remove(bucket) };
                        crate::Entry::Vacant(VacantEntry {
                            cache: self,
                            shard,
                            slot,
                            hash,
                            shard_index,
                        })
                    }
                }
            }
            Err(slot) => crate::Entry::Vacant(VacantEntry {
                cache: self,
                shard,
                slot,
                hash,
                shard_index,
            }),
        }
    }
}

impl<T, Lv, Ls, S: BuildHasher> SyncCache<T, Lv, Ls, S>
where
    Ls: ShardLayer<Pointer<T, Lv>, Value = Lv>,
    S: BuildHasher,
{
    fn hash_and_shard(&self, key: &(impl Hash + ?Sized)) -> (u64, usize) {
        let hash = self.hash_builder.hash_one(key);
        let shard = hash ^ hash.rotate_right(u64::BITS / 2);
        let shard = (shard as usize) & self.mask;
        (hash, shard)
    }

    // fn check_read(&self, shard_index: usize) {
    //     match Ls::READ_LOCK_BEHAVIOR {
    //         ReadLockBehavior::ReadLockOnly => {
    //             let shard = self.shards[shard_index].read();
    //             let pointer = shard
    //                 .values
    //                 .get(hash, |p| p.0.value.key().borrow() == key)?
    //                 .clone();

    //             let mut cell = UpgradeReadGuardCell::new(shard);
    //             let layer_shard =
    //                 MapUpgradeReadGuard::new(cell.borrow_ref(), |s| &s.layer, |s| &mut s.layer);
    //             match Ls::read::<ResolveLayer>(layer_shard, &pointer) {
    //                 ReadResult::Retain => Some(pointer),
    //                 ReadResult::Remove => {
    //                     let mut shard = cell.borrow_ref().upgrade();
    //                     // XX: need to re-look up because someone else might've beat us to the write lock
    //                     let pointer = shard
    //                         .values
    //                         .remove_entry(hash, |p| p.0.value.key().borrow() == key)?;
    //                     shard.layer.remove::<ResolveLayer>(&pointer);
    //                     Some(pointer)
    //                 }
    //             }
    //         }

    //         ReadLockBehavior::RequireWriteLock => {
    //             let mut shard = self.shards[shard].write();
    //             let bucket = shard
    //                 .values
    //                 .find(hash, |p| p.0.value.key().borrow() == key)?;
    //             // XX: safety
    //             let pointer = unsafe { bucket.as_ref() };
    //             match Ls::read::<ResolveLayer>(&mut shard.layer, &pointer) {
    //                 ReadResult::Retain => Some(pointer.clone()),
    //                 ReadResult::Remove => {
    //                     shard.layer.remove::<ResolveLayer>(pointer);
    //                     // XX: safety
    //                     unsafe {
    //                         shard.values.remove(bucket);
    //                     }
    //                     None
    //                 }
    //             }
    //         }
    //     }
    // }
}

struct OccupiedEntry<'a, T: crate::Value, Lv, Ls, S> {
    cache: &'a SyncCache<T, Lv, Ls, S>,
    shard: RwLockWriteGuard<'a, Shard<T, Lv, Ls>>,
    shard_index: usize,
    bucket: Bucket<Pointer<T, Lv>>,
}

impl<T: crate::Value, Lv, Ls, S> OccupiedEntry<'_, T, Lv, Ls, S> {
    fn pointer_ref(&self) -> &Pointer<T, Lv> {
        // XX Safety
        unsafe { self.bucket.as_ref() }
    }
}

impl<T, Lv, Ls, S> crate::OccupiedEntry for OccupiedEntry<'_, T, Lv, Ls, S>
where
    T: crate::Value + 'static,
    Ls: ShardLayer<Pointer<T, Lv>, Value = Lv>,
    S: BuildHasher,
{
    type Pointer = Pointer<T, Lv>;

    fn pointer(&self) -> Pointer<T, Lv> {
        self.pointer_ref().clone()
    }

    fn value(&self) -> &T {
        &self.pointer_ref()
    }

    fn replace(mut self, value: T) -> Pointer<T, Lv> {
        // XX Safety
        let pointer = unsafe { self.bucket.as_mut() };
        debug_assert!(value.key() == pointer.key());

        self.shard.layer.remove::<ResolveLayer>(pointer);
        let shard = &mut *self.shard;
        let replace = shard.layer.write::<ResolveLayer>(Write {
            cache: self.cache,
            shard_values: &mut shard.values,
            shard_index: self.shard_index,
            target: value,
        });
        *pointer = replace.clone();

        replace
    }

    fn remove(mut self) -> Pointer<T, Lv> {
        // XX Safety
        let (removed, _slot) = unsafe { self.shard.values.remove(self.bucket) };
        self.shard.layer.remove::<ResolveLayer>(&removed);
        removed
    }
}

struct Write<'a, T, Lv, Ls, S> {
    cache: &'a SyncCache<T, Lv, Ls, S>,
    shard_values: &'a mut RawTable<Pointer<T, Lv>>,
    shard_index: usize,
    target: T,
}

impl<T, Lv, Ls, S> crate::layer::Write<Pointer<T, Lv>, Lv> for Write<'_, T, Lv, Ls, S>
where
    T: crate::Value,
    Ls: ShardLayer<Pointer<T, Lv>, Value = Lv>,
    S: BuildHasher,
{
    fn target(&self) -> &<Pointer<T, Lv> as Deref>::Target {
        &self.target
    }

    fn remove(&mut self, pointer: &Pointer<T, Lv>) {
        let (hash, shard_index) = self.cache.hash_and_shard(pointer.key());
        debug_assert_eq!(shard_index, self.shard_index);

        self.shard_values
            .remove_entry(hash, |p| Arc::ptr_eq(&p.0, &pointer.0))
            .expect("layer shard and map out of sync");
    }

    fn insert(self, layer: Lv) -> Pointer<T, Lv> {
        Pointer(Arc::new(Value {
            value: self.target,
            layer,
        }))
    }
}

struct VacantEntry<'a, T, Lv, Ls, S> {
    cache: &'a SyncCache<T, Lv, Ls, S>,
    shard: RwLockWriteGuard<'a, Shard<T, Lv, Ls>>,
    shard_index: usize,
    slot: InsertSlot,
    hash: u64,
}

impl<T, Lv, Ls, S> crate::VacantEntry for VacantEntry<'_, T, Lv, Ls, S>
where
    T: crate::Value + 'static,
    Ls: ShardLayer<Pointer<T, Lv>, Value = Lv>,
    S: BuildHasher,
{
    type Pointer = Pointer<T, Lv>;

    fn insert(mut self, value: T) -> Pointer<T, Lv> {
        debug_assert_eq!(self.hash, self.cache.hash_builder.hash_one(value.key()));

        let shard = &mut *self.shard;
        let insert = shard.layer.write::<ResolveLayer>(Write {
            cache: self.cache,
            shard_values: &mut shard.values,
            shard_index: self.shard_index,
            target: value,
        });

        // XX: Safety
        unsafe {
            self.shard
                .values
                .insert_in_slot(self.hash, self.slot, insert.clone());
        }

        insert
    }
}
