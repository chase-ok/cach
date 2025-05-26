use std::{
    borrow::Borrow,
    cmp::Eq,
    hash::{BuildHasher, Hash},
    iter::FusedIterator,
    ops::Deref,
    ptr,
    sync::Arc,
};

use arc_swap::{ArcSwap, ArcSwapAny, AsRaw, Guard};
use crossbeam_utils::CachePadded;
use hashbrown::{hash_table, DefaultHashBuilder, HashTable};
use parking_lot::RwLock;
use ref_cast::RefCast;
use stable_deref_trait::{CloneStableDeref, StableDeref};

use crate::atomic::ComputeError;

use super::{Compute, Mutate};

pub struct Builder<S = DefaultHashBuilder> {
    hash_builder: S,
}

impl<S: BuildHasher> super::Builder for Builder<S> {
    type Cache<T: crate::Value> = Cache<T, S>;

    fn build<T: crate::Value>(self) -> Self::Cache<T> {
        Cache {
            shards: (0..16)
                .map(|_| {
                    CachePadded::new(RwLock::new(Shard {
                        values: HashTable::with_capacity(16),
                    }))
                })
                .collect(),
            hash_builder: self.hash_builder,
            mask: 16 - 1,
        }
    }
}

pub struct Cache<T, S = DefaultHashBuilder> {
    shards: Vec<CachePadded<RwLock<Shard<T>>>>,
    hash_builder: S,
    mask: usize,
}

struct Shard<T> {
    values: HashTable<ArcSwap<T>>,
}

#[derive(RefCast)]
#[repr(transparent)]
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
    T: crate::Value,
    S: BuildHasher,
{
    type Pointer = Pointer<T>;

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
        struct Iter<S, T> {
            shards: S,
            pointers: Vec<Arc<T>>,
        }

        impl<'a, S, T: 'a> Iterator for Iter<S, T>
        where
            S: Iterator<Item = &'a RwLock<Shard<T>>>,
        {
            type Item = Pointer<T>;

            fn next(&mut self) -> Option<Self::Item> {
                while self.pointers.is_empty() {
                    if let Some(shard) = self.shards.next() {
                        let shard = shard.read();
                        self.pointers.extend(shard.values.iter().map(ArcSwap::load_full));
                    } else {
                        break;
                    }
                }

                self.pointers.pop().map(Pointer)
            }
        }

        impl<'a, S, T: 'a> FusedIterator for Iter<S, T> where
            S: Iterator<Item = &'a RwLock<Shard<T>>> + FusedIterator
        {
        }

        // XX could use smaller chunks for consistent performance
        Iter {
            shards: self.shards.iter().map(|s| &**s),
            pointers: Vec::new(),
        }
    }

    fn insert(&self, value: T) -> Self::Pointer {
        let value = Arc::new(value);
        let key = value.key();
        let (hash, shard) = self.hash_and_shard(key);

        if let Some(swap) = self.shards[shard]
            .read()
            .values
            .find(hash, |s| s.load().key() == key)
        {
            swap.store(value.clone());
        } else {
            let mut shard = self.shards[shard].write();
            match shard.values.entry(hash, |s| s.load().key() == key, |s| self.hash_builder.hash_one(s.load().key())) {
                hash_table::Entry::Occupied(occupied) => {
                    occupied.get().store(value.clone());
                }
                hash_table::Entry::Vacant(vacant) => {
                    vacant.insert(ArcSwapAny::from(value.clone()));
                }
            }
        }

        Pointer(value)
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
        match shard.values.find_entry(hash, |s| s.load().key().borrow() == key) {
            Ok(occupied) => {
                let value = occupied.get().load_full();
                if f(&value) {
                    occupied.remove();
                    Ok(Pointer(value))
                } else {
                    Err(Some(Pointer(value)))
                }
            },
            Err(_) => Err(None),
        }
    }
}

enum ComputeKind<'a, T, K: ?Sized> {
    ByValue(T),
    ByKey(&'a K),
}

impl<T: crate::Value, S: BuildHasher> Cache<T, S> {
    #[inline]
    fn do_compute<K, R>(
        &self,
        kind: ComputeKind<'_, T, K>,
        mut f: impl FnMut(Option<T>, Option<&Pointer<T>>) -> Mutate<T, R>,
    ) -> Compute<Pointer<T>, R>
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
            let swap = shard.values.find(hash, |s| s.load().key().borrow() == key);
            let mut value = match kind {
                ComputeKind::ByValue(v) => Some(v),
                ComputeKind::ByKey(_) => None,
            };

            if let Some(swap) = swap {
                let mut current = swap.load();
                loop {
                    match f(value.take(), Some(Pointer::ref_cast(&current))) {
                        Mutate::None(r) => return Compute::None(r),
                        Mutate::Insert(new) => {
                            let new = Arc::new(new);
                            let prev = swap.compare_and_swap(&current, new.clone());
                            if ptr::eq(current.as_raw(), prev.as_raw()) {
                                return Compute::Overwrote {
                                    before: Pointer(Guard::into_inner(current)),
                                    after: Pointer(new),
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

        match shard.values.entry(
            hash,
            |s| s.load().key() == key,
            |s| self.hash_builder.hash_one(s.load().key()),
        ) {
            hash_table::Entry::Occupied(occupied) => {
                // XX safety
                let current = occupied.get().load();

                let mutate = match (expected, mutate) {
                    (Some(expected), m) if Arc::ptr_eq(&current, &expected) => m,
                    (_, Mutate::Insert(v)) => f(Some(v), Some(Pointer::ref_cast(&current))),
                    (_, Mutate::Remove) => f(None, Some(Pointer::ref_cast(&current))),
                    _ => unreachable!(),
                };

                match mutate {
                    Mutate::None(r) => Compute::None(r),
                    Mutate::Insert(new) => {
                        let new = Arc::new(new);
                        occupied.get().store(new.clone());
                        Compute::Overwrote {
                            before: Pointer(Guard::into_inner(current)),
                            after: Pointer(new),
                        }
                    }
                    Mutate::Remove => {
                        occupied.remove();
                        Compute::Removed(Pointer(Guard::into_inner(current)))
                    }
                }
            }
            hash_table::Entry::Vacant(vacant) => {
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
                        vacant.insert(new.clone().into());
                        Compute::Inserted(Pointer(new))
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
