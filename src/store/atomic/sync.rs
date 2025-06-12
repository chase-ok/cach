use std::{
    hash::{BuildHasher, Hash},
    ops::Deref,
    sync::Arc,
};

use arc_swap::{ArcSwap, ArcSwapAny, Guard};
use crossbeam_utils::CachePadded;
use equivalent::Equivalent;
use hashbrown::{DefaultHashBuilder, HashTable, hash_table};
use parking_lot::{RawRwLock, RwLock};
use stable_deref_trait::{CloneStableDeref, StableDeref};

use crate::{
    lock::{RwLockReadGuardDetached, RwLockWriteGuardDetached},
    store::{
        BuildStore, Entry, Store,
        hash::{self, OccupiedEntry as _, VacantEntry as _},
        strategy::{BuildStrategy, Insert, InsertShared, Lock, Strategy, StrategyPointer},
    },
};

pub struct Builder<H = DefaultHashBuilder> {
    hash_builder: H,
}

impl<H: BuildHasher> BuildStore for Builder<H> {
    type Store<T, S>
        = SyncStore<T, S::Strategy<Pointer<T, S::Value>>, S::Value, H>
    where
        T: hash::Value + Send + Sync + 'static,
        S: BuildStrategy<T>;

    fn build_store_with_strategy<T, S>(self, strategy: S) -> Self::Store<T, S>
    where
        T: 'static + hash::Value + Send + Sync,
        S: BuildStrategy<T>,
    {
        todo!()
    }
}

pub struct Pointer<T, Sv>(Arc<Value<T, Sv>>);

impl<T, Sv> Clone for Pointer<T, Sv> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T, Sv> Deref for Pointer<T, Sv> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0.user
    }
}

// XX
unsafe impl<T, Sv> StableDeref for Pointer<T, Sv> { }
unsafe impl<T, Sv> CloneStableDeref for Pointer<T, Sv> { }

impl<T, Sv> StrategyPointer for Pointer<T, Sv> {
    type StrategyValue = Sv;

    fn strategy_value(&self) -> &Self::StrategyValue {
        &self.0.strategy
    }
}

struct Value<T, Sv> {
    user: T,
    strategy: Sv,
}

pub struct SyncStore<T, S, Sv, H> {
    shards: Vec<CachePadded<RwLock<Shard<T, S, Sv>>>>,
    hash_builder: H,
    mask: usize,
}

struct Shard<T, S, Sv> {
    values: HashTable<ArcSwap<Value<T, Sv>>>,
    strategy: S,
}

impl<T, S, Sv, H> Store<T> for SyncStore<T, S, Sv, H>
where
    T: hash::Value + Send + Sync + 'static,
    S: Strategy<Pointer<T, Sv>, Value = Sv>,
    Sv: Send + Sync + 'static,
    H: BuildHasher,
{
    type Pointer = Pointer<T, Sv>;

    fn len(&self) -> usize {
        self.shards.iter().map(|s| s.read().values.len()).sum()
    }

    fn iter(&self) -> impl Iterator<Item = Self::Pointer> {
        std::iter::empty()
    }

    fn extract_if(
        &self,
        f: impl FnMut(&Self::Pointer) -> bool,
    ) -> impl Iterator<Item = Self::Pointer> {
        std::iter::empty()
    }

    fn insert(&self, value: T) -> Self::Pointer {
        let entry = match S::INSERT_LOCK {
            Lock::Shared => self.entry_shared(value.key()),
            Lock::Exclusive => self.entry_exclusive(value.key()),
        };
        match entry {
            Entry::Occupied(o) => o.insert(value),
            Entry::Vacant(v) => v.insert(value),
        }
    }

    #[inline]
    fn or_insert(&self, value: T) -> Self::Pointer {
        hash::impl_or_insert(self, value)
    }

    #[inline]
    fn remove(&self, value: &T) -> Option<Self::Pointer> {
        match self.entry_exclusive(value.key()) {
            Entry::Occupied(o) => o.remove_key(),
            Entry::Vacant(_) => None,
        }
    }

    fn upsert(&self, value: T, f: impl for<'a> FnMut(&'a T, &'a Self::Pointer) -> &'a T) -> Self::Pointer {
        todo!()
    }

    // #[inline]
    // fn upsert(&self, value: T, f: impl FnMut(T, &Self::Pointer) -> Option<T>) -> Self::Pointer {
    //     hash::impl_upsert(self, value, f)
    // }
}

impl<T, S, Sv, H> hash::Store<T> for SyncStore<T, S, Sv, H>
where
    T: hash::Value + Send + Sync + 'static,
    S: Strategy<Pointer<T, Sv>, Value = Sv>,
    Sv: Send + Sync + 'static,
    H: BuildHasher,
{
    #[allow(refining_impl_trait)]
    #[inline]
    fn entry<'a, K>(
        &'a self,
        key: &K,
    ) -> Entry<OccupiedEntry<'a, T, S, Sv, H>, VacantEntry<'a, T, S, Sv, H>>
    where
        K: ?Sized + Hash + Equivalent<T::Key>,
    {
        self.entry_shared(key)
    }

    #[inline]
    fn remove_key(&self, key: &(impl ?Sized + Hash + Equivalent<T::Key>)) -> Option<Self::Pointer> {
        match self.entry_exclusive(key) {
            Entry::Occupied(o) => o.remove_key(),
            Entry::Vacant(_) => None,
        }
    }
}

impl<T: hash::Value + 'static, S, Sv: 'static, H: BuildHasher> SyncStore<T, S, Sv, H>
where
    S: Strategy<Pointer<T, Sv>, Value = Sv>,
{
    fn hash_and_shard(&self, key: &(impl ?Sized + Hash)) -> (u64, usize) {
        let hash = self.hash_builder.hash_one(key);
        let shard = hash ^ hash.rotate_right(u64::BITS / 2);
        let shard = (shard as usize) & self.mask;
        (hash, shard)
    }

    fn entry_shared<'a>(
        &'a self,
        key: &(impl ?Sized + Hash + Equivalent<T::Key>),
    ) -> Entry<OccupiedEntry<'a, T, S, Sv, H>, VacantEntry<'a, T, S, Sv, H>> {
        let (hash, shard_index) = self.hash_and_shard(key);
        let shard = self.shards[shard_index].read();

        // SAFETY: The data will not outlive the guard, since we pass the guard to `Entry`.
        let (guard, shard) = unsafe { RwLockReadGuardDetached::detach_from(shard) };

        match shard
            .values
            .find(hash, |v| key.equivalent(v.load().user.key()))
        {
            Some(swap) => Entry::Occupied(OccupiedEntry(OccupiedEntryInner::Shared {
                pointer: Pointer(swap.load_full()),
                hash,
                shard_index,
                shard,
                store: self,
                swap,
                guard,
            })),
            None => Entry::Vacant(VacantEntry(VacantEntryInner::Shared {
                hash,
                shard_index,
                store: self,
            })),
        }
    }

    fn entry_exclusive<'a>(
        &'a self,
        key: &(impl ?Sized + Hash + Equivalent<T::Key>),
    ) -> Entry<OccupiedEntry<'a, T, S, Sv, H>, VacantEntry<'a, T, S, Sv, H>> {
        let (hash, shard) = self.hash_and_shard(key);
        self.entry_exclusive_with_hash_and_shard(key, hash, shard)
    }

    fn entry_exclusive_with_hash_and_shard<'a>(
        &'a self,
        key: &(impl ?Sized + Hash + Equivalent<T::Key>),
        hash: u64,
        shard: usize,
    ) -> Entry<OccupiedEntry<'a, T, S, Sv, H>, VacantEntry<'a, T, S, Sv, H>> {
        let mut shard = self.shards[shard].write();

        // always do some amount of purging when we get the write lock!
        shard.purge(&self.hash_builder);

        // SAFETY: The data will not outlive the guard, since we pass the guard to `Entry`.
        let (guard, shard) = unsafe { RwLockWriteGuardDetached::detach_from(shard) };
        match shard.values.entry(
            hash,
            |v| key.equivalent(v.load().user.key()),
            |v| self.hash_builder.hash_one(v.load().user.key()),
        ) {
            hash_table::Entry::Occupied(occupied) => {
                Entry::Occupied(OccupiedEntry(OccupiedEntryInner::Exclusive {
                    strategy: &mut shard.strategy,
                    pointer: Pointer(occupied.get().load_full()),
                    occupied,
                    _guard: guard,
                }))
            }
            hash_table::Entry::Vacant(vacant) => {
                Entry::Vacant(VacantEntry(VacantEntryInner::Exclusive {
                    strategy: &mut shard.strategy,
                    vacant,
                    guard,
                }))
            }
        }
    }
}

impl<T: hash::Value + 'static, S, Sv: 'static> Shard<T, S, Sv>
where
    S: Strategy<Pointer<T, Sv>, Value = Sv>,
{
    fn purge(&mut self, hash_builder: &impl BuildHasher) {
        // always do some amount of purging when we get the write lock!
        for pointer in self.strategy.purge() {
            let hash = hash_builder.hash_one(pointer.key());
            if let Ok(entry) = self
                .values
                .find_entry(hash, |v| v.load().user.key() == pointer.key())
            {
                entry.remove();
            } else {
                // should we warn/panic on missing?
            }
        }
    }
}

pub struct OccupiedEntry<'a, T, S, Sv, H>(OccupiedEntryInner<'a, T, S, Sv, H>);

enum OccupiedEntryInner<'a, T, S, Sv, H> {
    None,
    Shared {
        pointer: Pointer<T, Sv>,
        hash: u64,
        shard_index: usize,
        shard: &'a Shard<T, S, Sv>,
        store: &'a SyncStore<T, S, Sv, H>,
        swap: &'a ArcSwapAny<Arc<Value<T, Sv>>>,
        guard: RwLockReadGuardDetached<'a, RawRwLock>,
    },
    Exclusive {
        strategy: &'a mut S,
        pointer: Pointer<T, Sv>,
        occupied: hash_table::OccupiedEntry<'a, ArcSwapAny<Arc<Value<T, Sv>>>>,
        _guard: RwLockWriteGuardDetached<'a, RawRwLock>,
    },
}

impl<'a, T, S, Sv, H> hash::OccupiedEntry<'a> for OccupiedEntry<'a, T, S, Sv, H>
where
    T: hash::Value + Send + Sync + 'static,
    S: Strategy<Pointer<T, Sv>, Value = Sv>,
    Sv: Send + Sync + 'static,
    H: BuildHasher,
{
    type Value = T;
    type Pointer = Pointer<T, Sv>;
    type VacantEntry = VacantEntry<'a, T, S, Sv, H>;

    fn get(&self) -> &T {
        &*self.pointer()
    }

    fn pointer(&self) -> &Self::Pointer {
        match &self.0 {
            OccupiedEntryInner::None => unreachable!(),
            OccupiedEntryInner::Shared { pointer, .. } => &pointer,
            OccupiedEntryInner::Exclusive { pointer, .. } => &pointer,
        }
    }

    fn into_pointer(self) -> Self::Pointer {
        match self.0 {
            OccupiedEntryInner::None => unreachable!(),
            OccupiedEntryInner::Shared { pointer, .. } => pointer,
            OccupiedEntryInner::Exclusive { pointer, .. } => pointer,
        }
    }

    fn remove(mut self) -> Result<Self::Pointer, Entry<Self, Self::VacantEntry>> {
        // XX do this to release lock guard without chance of still using refs
        let (current, store, hash, shard) =
            match std::mem::replace(&mut self.0, OccupiedEntryInner::None) {
                OccupiedEntryInner::None => unreachable!(),
                OccupiedEntryInner::Shared {
                    pointer,
                    hash,
                    shard_index,
                    store,
                    ..
                } => (pointer, store, hash, shard_index),
                OccupiedEntryInner::Exclusive {
                    strategy,
                    pointer,
                    occupied,
                    ..
                } => {
                    strategy.remove(&pointer);
                    occupied.remove();
                    return Ok(pointer);
                }
            };

        match store.entry_exclusive_with_hash_and_shard(current.key(), hash, shard) {
            Entry::Occupied(OccupiedEntry(OccupiedEntryInner::Exclusive {
                strategy,
                pointer,
                occupied,
                ..
            })) if Arc::ptr_eq(&pointer.0, &current.0) => {
                strategy.remove(&pointer);
                occupied.remove();
                Ok(pointer)
            }
            entry => Err(entry),
        }
    }

    fn try_insert(
        mut self,
        value: T,
    ) -> Result<Self::Pointer, (T, Entry<Self, Self::VacantEntry>)> {
        // XX do this to release lock guard without chance of still using refs
        let (pointer, store, hash, shard) =
            match std::mem::replace(&mut self.0, OccupiedEntryInner::None) {
                OccupiedEntryInner::None => unreachable!(),

                OccupiedEntryInner::Shared {
                    pointer,
                    hash,
                    shard_index,
                    shard,
                    store,
                    swap,
                    guard,
                } => {
                    if S::INSERT_LOCK == Lock::Shared
                        && shard.strategy.start_insert_shared(&value) == InsertShared::Allow
                    {
                        let next = Pointer(Arc::new(Value {
                            strategy: shard.strategy.create_insert_shared_value(&value),
                            user: value,
                        }));

                        let swapped = swap.compare_and_swap(&pointer.0, next.0.clone());

                        return if Arc::ptr_eq(&pointer.0, &swapped) {
                            shard.strategy.complete_insert_shared(&pointer);
                            Ok(pointer)
                        } else {
                            let value = Arc::into_inner(pointer.0).unwrap();
                            shard
                                .strategy
                                .fail_insert_shared(&value.user, value.strategy);
                            Err((
                                value.user,
                                Entry::Occupied(OccupiedEntry(OccupiedEntryInner::Shared {
                                    pointer: Pointer(Guard::into_inner(swapped)),
                                    hash,
                                    shard_index,
                                    shard,
                                    store,
                                    swap,
                                    guard,
                                })),
                            ))
                        };
                    }

                    (pointer, store, hash, shard_index)
                }

                OccupiedEntryInner::Exclusive {
                    strategy,
                    pointer,
                    occupied,
                    ..
                } => {
                    strategy.remove(&pointer);

                    let pointer = Pointer(Arc::new(Value {
                        strategy: strategy.create_insert_value(&value),
                        user: value,
                    }));
                    occupied.get().store(pointer.clone().0);
                    strategy.complete_insert(&pointer);
                    return Ok(pointer);
                }
            };

        Err((
            value,
            store.entry_exclusive_with_hash_and_shard(pointer.key(), hash, shard),
        ))
    }
}

pub struct VacantEntry<'a, T, S, Sv, H>(VacantEntryInner<'a, T, S, Sv, H>);

enum VacantEntryInner<'a, T, S, Sv, H> {
    None,
    Shared {
        hash: u64,
        shard_index: usize,
        store: &'a SyncStore<T, S, Sv, H>,
    },
    Exclusive {
        strategy: &'a mut S,
        vacant: hash_table::VacantEntry<'a, ArcSwapAny<Arc<Value<T, Sv>>>>,
        guard: RwLockWriteGuardDetached<'a, RawRwLock>,
    },
}

impl<'a, T, S, Sv, H> hash::VacantEntry<'a> for VacantEntry<'a, T, S, Sv, H>
where
    T: hash::Value + Send + Sync + 'static,
    S: Strategy<Pointer<T, Sv>, Value = Sv>,
    Sv: Send + Sync + 'static,
    H: BuildHasher,
{
    type Value = T;
    type Pointer = Pointer<T, Sv>;
    type OccupiedEntry = OccupiedEntry<'a, T, S, Sv, H>;

    fn try_insert(
        mut self,
        value: Self::Value,
    ) -> Result<Self::Pointer, (Self::Value, Self::OccupiedEntry)> {
        let (strategy, vacant, _guard) =
            match std::mem::replace(&mut self.0, VacantEntryInner::None) {
                VacantEntryInner::None => unreachable!(),
                VacantEntryInner::Shared {
                    hash,
                    shard_index: shard,
                    store,
                } => {
                    let key = value.key();
                    debug_assert_eq!((hash, shard), store.hash_and_shard(key));
                    match store.entry_exclusive_with_hash_and_shard(key, hash, shard) {
                        Entry::Occupied(occupied) => return Err((value, occupied)),
                        Entry::Vacant(VacantEntry(VacantEntryInner::Exclusive {
                            strategy,
                            vacant,
                            guard,
                        })) => (strategy, vacant, guard),
                        Entry::Vacant(_) => unreachable!(),
                    }
                }
                VacantEntryInner::Exclusive {
                    strategy,
                    vacant,
                    guard,
                } => (strategy, vacant, guard),
            };

        assert_eq!(
            strategy.start_insert(&value),
            Insert::Allow,
            "already purged"
        );

        let strategy_value = strategy.create_insert_value(&value);
        let pointer = Pointer(Arc::new(Value {
            user: value,
            strategy: strategy_value,
        }));
        vacant.insert(pointer.clone().0.into());
        strategy.complete_insert(&pointer);
        Ok(pointer)
    }
}
