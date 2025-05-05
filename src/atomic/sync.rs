use std::{
    borrow::Borrow,
    hash::{BuildHasher, Hash},
    ops::Deref,
    ptr,
    sync::Arc,
};

use arc_swap::{ArcSwap, ArcSwapAny, AsRaw, Guard};
use crossbeam_utils::CachePadded;
use hashbrown::{hash_map::DefaultHashBuilder, raw::RawTable};
use parking_lot::RwLock;
use stable_deref_trait::{CloneStableDeref, StableDeref};

use crate::atomic::ComputeError;

use super::{Compute, Mutate};

pub struct Cache<T, S = DefaultHashBuilder> {
    shards: Vec<CachePadded<RwLock<Shard<T>>>>,
    hash_builder: S,
    mask: usize,
    capacity_per_shard: usize,
}

struct Shard<T> {
    values: RawTable<ArcSwap<T>>,
}

pub struct Pointer<T>(Arc<T>);

impl<T> Clone for Pointer<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Deref for Pointer<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// XX: just a wrapper around Arc<> that does impl Stable/Clone
unsafe impl<T> StableDeref for Pointer<T> {}
unsafe impl<T> CloneStableDeref for Pointer<T> {}

impl<T, S> super::Cache<T> for Cache<T, S>
where
    T: crate::Value + 'static,
    T::Key: Hash + std::cmp::Eq,
    S: BuildHasher,
{
    type Pointer = Arc<T>;

    fn len(&self) -> usize {
        self.shards
            .iter()
            .map(|shard| shard.read().values.len())
            .sum()
    }

    fn compute<R>(
        &self,
        value: T,
        f: impl FnMut(Option<T>, Option<&Self::Pointer>) -> Mutate<T, R>,
    ) -> Compute<Self::Pointer, R> {
        self.do_compute::<T::Key, R>(ComputeKind::ByValue(value), f)
    }

    fn compute_key<K, R>(
        &self,
        key: &K,
        f: impl FnMut(Option<T>, Option<&Self::Pointer>) -> Mutate<T, R>,
    ) -> Compute<Self::Pointer, R>
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        self.do_compute(ComputeKind::ByKey(key), f)
    }

    fn iter(&self) -> impl Iterator<Item = Self::Pointer> {
        todo!();
        [].into_iter()
        // self.shards.iter().flat_map(|shard| {
        //     let mut pointers = Vec::new();
        //     loop {
        //         pointers.clear();

        //         // XX
        //         let buckets_len = {
        //             let shard = shard.read();
        //             pointers.reserve(shard.values.len());
        //             shard.values.buckets()
        //         };

        //         const CHUNK: usize = 256;
        //         let mut i = 0;
        //         while i < buckets_len {
        //             match Ls::ITER_READ_LOCK {
        //                 layer::ReadLock::None => {
        //                     let shard = shard.read();
        //                     for bucket in i..buckets_len.min(i + CHUNK) {
        //                         // XX safety
        //                         if unsafe { shard.values.is_bucket_full(bucket) } {
        //                             // XX safety
        //                             let bucket = unsafe { shard.values.bucket(bucket) };
        //                             // XX safety
        //                             let pointer = unsafe { bucket.as_ref() }.clone();
        //                             pointers.push(pointer);
        //                         }
        //                     }
        //                 }
        //                 layer::ReadLock::Ref | layer::ReadLock::Mut => {
        //                     let mut shard = shard.write(); // don't try to upgrade later to a write lock on ::Remove
        //                     for bucket in i..buckets_len.min(i + CHUNK) {
        //                         // XX safety
        //                         if unsafe { shard.values.is_bucket_full(bucket) } {
        //                             // XX safety
        //                             let bucket = unsafe { shard.values.bucket(bucket) };
        //                             // XX safety
        //                             let pointer = unsafe { bucket.as_ref() };
        //                             match shard.layer.iter_read_mut::<ResolveLayer>(pointer) {
        //                                 ReadResult::Retain => pointers.push(pointer.clone()),
        //                                 ReadResult::Remove => {
        //                                     shard.layer.remove::<ResolveLayer>(pointer);
        //                                     unsafe {
        //                                         shard.values.remove(bucket);
        //                                     }
        //                                 }
        //                             }
        //                         }
        //                     }
        //                 }
        //             }

        //             i += CHUNK
        //         }
        //         break;
        //     }
        //     pointers
        // })
    }

    fn insert(&self, value: T) -> Self::Pointer {
        let value = Arc::new(value);
        let key = value.key();
        let (hash, shard) = self.hash_and_shard(key);

        if let Some(swap) = self.shards[shard]
            .read()
            .values
            .get(hash, |s| s.load().key() == key)
        {
            swap.store(value.clone());
        } else {
            let mut shard = self.shards[shard].write();
            match shard.values.find_or_find_insert_slot(
                hash,
                |s| s.load().key() == key,
                |s| self.hash_builder.hash_one(s.load().key()),
            ) {
                Ok(bucket) => {
                    let swap = unsafe { bucket.as_ref() };
                    swap.store(value.clone());
                }
                Err(slot) => unsafe {
                    shard
                        .values
                        .insert_in_slot(hash, slot, ArcSwapAny::from(value.clone()));
                },
            }
        }

        value
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
        let (hash, shard) = self.hash_and_shard(key);
        let mut shard = self.shards[shard].write();
        match shard.values.find(hash, |s| s.load().key().borrow() == key) {
            Some(bucket) => {
                let swap = unsafe { bucket.as_ref() };
                let current = swap.load();
                if f(&current) {
                    unsafe {
                        shard.values.remove(bucket);
                    }
                    Ok(Guard::into_inner(current))
                } else {
                    Err(Some(Guard::into_inner(current)))
                }
            }
            None => Err(None),
        }
    }
}

enum ComputeKind<'a, T, K: ?Sized> {
    ByValue(T),
    ByKey(&'a K),
}

impl<T: crate::Value, S: BuildHasher> Cache<T, S> {
    fn do_compute<K, R>(
        &self,
        kind: ComputeKind<'_, T, K>,
        mut f: impl FnMut(Option<T>, Option<&Arc<T>>) -> Mutate<T, R>,
    ) -> Compute<Arc<T>, R>
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        let key: &K = match &kind {
            ComputeKind::ByValue(v) => v.key().borrow(),
            ComputeKind::ByKey(k) => k,
        };
        let (hash, shard) = self.hash_and_shard(key);

        let (mutate, expected) = {
            let shard = self.shards[shard].read();
            let swap = shard.values.get(hash, |s| s.load().key().borrow() == key);
            let mut value = match kind {
                ComputeKind::ByValue(v) => Some(v),
                ComputeKind::ByKey(_) => None,
            };

            if let Some(swap) = swap {
                let mut current = swap.load();
                loop {
                    match f(value.take(), Some(&current)) {
                        Mutate::None(r) => return Compute::None(r),
                        Mutate::Insert(new) => {
                            let new = Arc::new(new);
                            let prev = swap.compare_and_swap(&current, new.clone());
                            if ptr::eq(current.as_raw(), prev.as_raw()) {
                                return Compute::Overwrote {
                                    before: Guard::into_inner(current),
                                    after: new,
                                };
                            } else {
                                value = Some(Arc::into_inner(new).unwrap());
                                current = prev;
                            }
                        }
                        Mutate::Remove => {
                            break (Mutate::Remove::<T, R>, Some(Guard::into_inner(current)));
                        }
                    }
                }
            } else {
                match f(value, None) {
                    Mutate::None(r) => return Compute::None(r),
                    Mutate::Insert(v) => (Mutate::Insert(v), None),
                    Mutate::Remove => {
                        return Compute::Err(ComputeError {
                            message: "removed non-existent value",
                        });
                    }
                }
            }
        };

        let mut shard = self.shards[shard].write();
        let key = match (&mutate, &expected) {
            (_, Some(v)) => v.key(),
            (Mutate::Insert(v), _) => v.key(),
            _ => unreachable!(),
        };

        match shard.values.find_or_find_insert_slot(
            hash,
            |s| s.load().key() == key,
            |s| self.hash_builder.hash_one(s.load().key()),
        ) {
            Ok(bucket) => {
                // XX safety
                let swap = unsafe { bucket.as_ref() };
                let current = swap.load();

                let mutate = match (expected, mutate) {
                    (Some(expected), m) if Arc::ptr_eq(&current, &expected) => m,
                    (_, Mutate::Insert(v)) => f(Some(v), Some(&current)),
                    (_, Mutate::Remove) => f(None, Some(&current)),
                    _ => unreachable!(),
                };

                match mutate {
                    Mutate::None(r) => Compute::None(r),
                    Mutate::Insert(new) => {
                        let new = Arc::new(new);
                        swap.store(new.clone());
                        Compute::Overwrote {
                            before: Guard::into_inner(current),
                            after: new,
                        }
                    }
                    Mutate::Remove => {
                        // XX Safety
                        unsafe {
                            shard.values.remove(bucket);
                        }
                        Compute::Removed(Guard::into_inner(current))
                    }
                }
            }

            Err(slot) => {
                let mutate = match (expected, mutate) {
                    (None, m) => m,
                    (_, Mutate::Insert(v)) => f(Some(v), None),
                    (_, Mutate::Remove) => f(None, None),
                    _ => unreachable!(),
                };

                match mutate {
                    Mutate::None(r) => Compute::None(r),
                    Mutate::Insert(new) => {
                        let new = Arc::new(new);
                        let swap = ArcSwapAny::from(new.clone());
                        unsafe {
                            shard.values.insert_in_slot(hash, slot, swap);
                        }
                        Compute::Inserted(new)
                    }
                    Mutate::Remove => Compute::Err(ComputeError {
                        message: "removed non-existent value",
                    }),
                }
            }
        }
    }

    fn hash_and_shard(&self, key: &(impl Hash + ?Sized)) -> (u64, usize) {
        let hash = self.hash_builder.hash_one(key);
        let shard = hash ^ hash.rotate_right(u64::BITS / 2);
        let shard = (shard as usize) & self.mask;
        (hash, shard)
    }
}

// struct OccupiedEntry<'a, T: crate::Value, Lv, Ls, S> {
//     cache: &'a SyncCache<T, Lv, Ls, S>,
//     shard: RwLockWriteGuard<'a, Shard<T, Lv, Ls>>,
//     shard_index: usize,
//     bucket: Bucket<Pointer<T, Lv>>,
// }

// impl<T: crate::Value, Lv, Ls, S> OccupiedEntry<'_, T, Lv, Ls, S> {
//     fn pointer_ref(&self) -> &Pointer<T, Lv> {
//         // XX Safety
//         unsafe { self.bucket.as_ref() }
//     }
// }

// impl<T, Lv, Ls, S> crate::OccupiedEntry for OccupiedEntry<'_, T, Lv, Ls, S>
// where
//     T: crate::Value + 'static,
//     Ls: ShardLayer<Pointer<T, Lv>, Value = Lv>,
//     S: BuildHasher,
// {
//     type Pointer = Pointer<T, Lv>;

//     fn pointer(&self) -> Pointer<T, Lv> {
//         self.pointer_ref().clone()
//     }

//     fn value(&self) -> &T {
//         &self.pointer_ref()
//     }

//     fn replace(mut self, value: T) -> Pointer<T, Lv> {
//         // XX Safety
//         let pointer = unsafe { self.bucket.as_mut() };
//         debug_assert!(value.key() == pointer.key());

//         self.shard.layer.remove::<ResolveLayer>(pointer);
//         let shard = &mut *self.shard;
//         let replace = shard.layer.write::<ResolveLayer>(Write {
//             cache: self.cache,
//             shard_values: &mut shard.values,
//             shard_index: self.shard_index,
//             target: value,
//         });
//         *pointer = replace.clone();

//         replace
//     }

//     fn remove(mut self) -> Pointer<T, Lv> {
//         // XX Safety
//         let (removed, _slot) = unsafe { self.shard.values.remove(self.bucket) };
//         self.shard.layer.remove::<ResolveLayer>(&removed);
//         removed
//     }
// }

// struct Write<'a, T, Lv, Ls, S> {
//     cache: &'a SyncCache<T, Lv, Ls, S>,
//     shard_values: &'a mut RawTable<Pointer<T, Lv>>,
//     shard_index: usize,
//     target: T,
// }

// impl<T, Lv, Ls, S> layer::Write<Pointer<T, Lv>, Lv> for Write<'_, T, Lv, Ls, S>
// where
//     T: crate::Value,
//     Ls: ShardLayer<Pointer<T, Lv>, Value = Lv>,
//     S: BuildHasher,
// {
//     fn target(&self) -> &<Pointer<T, Lv> as Deref>::Target {
//         &self.target
//     }

//     fn remove(&mut self, pointer: &Pointer<T, Lv>) {
//         let (hash, shard_index) = self.cache.hash_and_shard(pointer.key());
//         debug_assert_eq!(shard_index, self.shard_index);

//         self.shard_values
//             .remove_entry(hash, |p| Arc::ptr_eq(&p.0, &pointer.0))
//             .expect("layer shard and map out of sync");
//     }

//     fn write(self, layer: Lv) -> Pointer<T, Lv> {
//         Pointer(Arc::new(Value {
//             value: self.target,
//             layer,
//         }))
//     }
// }

// struct VacantEntry<'a, T, Lv, Ls, S> {
//     cache: &'a SyncCache<T, Lv, Ls, S>,
//     shard: RwLockWriteGuard<'a, Shard<T, Lv, Ls>>,
//     shard_index: usize,
//     slot: InsertSlot,
//     hash: u64,
// }

// impl<T, Lv, Ls, S> crate::VacantEntry for VacantEntry<'_, T, Lv, Ls, S>
// where
//     T: crate::Value + 'static,
//     Ls: ShardLayer<Pointer<T, Lv>, Value = Lv>,
//     S: BuildHasher,
// {
//     type Pointer = Pointer<T, Lv>;

//     fn insert(mut self, value: T) -> Pointer<T, Lv> {
//         debug_assert_eq!(self.hash, self.cache.hash_builder.hash_one(value.key()));

//         let shard = &mut *self.shard;
//         let insert = shard.layer.write::<ResolveLayer>(Write {
//             cache: self.cache,
//             shard_values: &mut shard.values,
//             shard_index: self.shard_index,
//             target: value,
//         });

//         // XX: Safety
//         unsafe {
//             self.shard
//                 .values
//                 .insert_in_slot(self.hash, self.slot, insert.clone());
//         }

//         insert
//     }
// }
