use std::{
    any::Any, cell::RefCell, hash::{BuildHasher, Hash}, marker::PhantomData, ops::Deref, sync::Arc
};

use arc_swap::ArcSwap;
use crossbeam_utils::CachePadded;
use equivalent::Equivalent;
use hashbrown::{DefaultHashBuilder, HashTable, hash_table};
use owning_ref::{OwningHandle, OwningRef};
// use parking_lot::{RwLock, RwLockWriteGuard};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use ref_cast::RefCast;
use yoke::Yoke;

use crate::store::{
    BuildStore, Entry, Store,
    hash::{self, OccupiedEntry as _},
    strategy::{
        BuildStrategy, Insert, InsertShared, Lock, RemoveShared, Strategy, StrategyPointer,
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

#[derive(RefCast)]
#[repr(transparent)]
#[doc(hidden)]
pub struct ErasedPointer<T, S> {
    inner: Arc<dyn Any + Send + Sync + 'static>,
    _marker: PhantomData<(T, S)>,
}

impl<T, V> Clone for ErasedPointer<T, V> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _marker: PhantomData,
        }
    }
}

impl<T: 'static, S: 'static> Deref for ErasedPointer<T, S> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        if let Some(pointer) = self.inner.downcast_ref::<(T, S)>() {
            &pointer.0
        } else {
            panic!()
        }
    }
}

impl<T: 'static, S: 'static> StrategyPointer for ErasedPointer<T, S> {
    type StrategyValue = S;

    fn strategy_value(&self) -> &Self::StrategyValue {
        if let Some(pointer) = self.inner.downcast_ref::<(T, S)>() {
            &pointer.1
        } else {
            panic!()
        }
    }
}

impl<T: 'static + Send + Sync, S: 'static + Send + Sync> ErasedPointer<T, S> {
    fn unerase_ref(pointer: &Pointer<T, S>) -> &Self {
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
    shards: Vec<CachePadded<RwLock<Shard<Value<T, Sv>, S>>>>,
    hash_builder: H,
    mask: usize,
}

struct Shard<T, S> {
    values: HashTable<ArcSwap<T>>,
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
        todo!()
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
        hash::impl_insert(self, value)
        // let key = value.key();
        // let (hash, shard_index) = self.hash_and_shard(key);

        // if S::INSERT_LOCK == Lock::Shared {
        //     let shard = self.shards[shard_index].read();
        //     match shard.strategy.start_insert_shared(&value) {
        //         InsertShared::Allow => {
        //             let strategy_value = shard.strategy.create_insert_shared_value(&value);

        //             if let Some(swap) = shard.values.find(hash, |v| v.load().user.key() == key) {
        //                 let pointer = Pointer(Arc::new(Value {
        //                     user: value,
        //                     strategy: strategy_value,
        //                 }));

        //                 let prev = Pointer(swap.swap(pointer.clone().0));

        //                 shard.strategy.complete_insert_shared(&pointer);
        //                 match shard.strategy.remove_shared(&prev) {
        //                     RemoveShared::Allow => {}
        //                     RemoveShared::RequireExclusive => {
        //                         // should we even allow this?
        //                         drop(shard);
        //                         self.shards[shard_index].write().strategy.remove(&prev);
        //                     }
        //                 }

        //                 return pointer;
        //             } else {
        //                 shard.strategy.fail_insert_shared(&value, strategy_value);
        //             }
        //         }
        //         _ => {}
        //     }
        // }

        // let mut shard = self.shards[shard_index].write();
        // let Shard { values, strategy } = &mut *shard;

        // // always do some amount of purging when we get the write lock!
        // for pointer in strategy.purge() {
        //     let hash = self.hash_builder.hash_one(pointer.key());
        //     if let Ok(entry) = values.find_entry(hash, |v| v.load().user.key() == pointer.key()) {
        //         entry.remove();
        //     } else {
        //         // should we warn/panic on missing?
        //     }
        // }

        // assert_eq!(
        //     strategy.start_insert(&value),
        //     Insert::Allow,
        //     "already purged"
        // );

        // let strategy_value = strategy.create_insert_value(&value);
        // match values.entry(
        //     hash,
        //     |v| v.load().user.key() == key,
        //     |v| self.hash_builder.hash_one(v.load().user.key()),
        // ) {
        //     hash_table::Entry::Occupied(occupied) => {
        //         let pointer = Pointer(Arc::new(Value {
        //             user: value,
        //             strategy: strategy_value,
        //         }));

        //         let prev = Pointer(occupied.get().swap(pointer.clone().0));
        //         strategy.remove(&prev);
        //         strategy.complete_insert(&pointer);
        //         pointer
        //     }
        //     hash_table::Entry::Vacant(vacant) => {
        //         let pointer = Pointer(Arc::new(Value {
        //             user: value,
        //             strategy: strategy_value,
        //         }));
        //         vacant.insert(pointer.clone().0.into());
        //         strategy.complete_insert(&pointer);
        //         pointer
        //     }
        // }
    }

    #[inline]
    fn or_insert(&self, value: T) -> Self::Pointer {
        hash::impl_or_insert(self, value)
    }

    #[inline]
    fn remove(&self, value: &T) -> Option<Self::Pointer> {
        match self.entry_mut(value.key()) {
            Entry::Occupied(o) => o.remove(),
            Entry::Vacant(_) => None,
        }
    }

    #[inline]
    fn upsert(&self, value: T, f: impl FnOnce(T, &Self::Pointer) -> Option<T>) -> Self::Pointer {
        hash::impl_upsert(self, value, f)
    }
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
    fn entry<'a, K>(&'a self, key: &K) -> Entry<OccupiedEntry<'a, T, Sv>, VacantEntry<'a, T, Sv>>
    where
        K: ?Sized + Hash + Equivalent<T::Key>,
    {
        self.entry_shared(key)
    }

    fn remove_key<K>(&self, key: &K) -> Option<Self::Pointer>
    where
        K: ?Sized + Hash + Equivalent<T::Key>,
    {
        match self.entry_mut(key) {
            Entry::Occupied(o) => o.remove(),
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
    ) -> Entry<OccupiedEntry<'a, T, Sv>, VacantEntry<'a, T, Sv>> {
        todo!()
    }

    fn entry_mut<'a>(
        &'a self,
        key: &(impl ?Sized + Hash + Equivalent<T::Key>),
    ) -> Entry<OccupiedEntry<'a, T, Sv>, VacantEntry<'a, T, Sv>> {
        let (hash, shard) = self.hash_and_shard(key);
        let mut shard = self.shards[shard].write().unwrap();
        let Shard { values, strategy } = &mut *shard;

        // always do some amount of purging when we get the write lock!
        for pointer in strategy.purge() {
            let hash = self.hash_builder.hash_one(pointer.key());
            if let Ok(entry) = values.find_entry(hash, |v| v.load().user.key() == pointer.key()) {
                entry.remove();
            } else {
                // should we warn/panic on missing?
            }
        }

        let lock = RwLock::new(RefCell::new(true));
        let handle = OwningHandle::new(lock.write().unwrap());
        // Yoke::new_always_owned(shard);
        // let handle = OwningHandle::new_mut(shard);
        // RwLockWriteGuard::map(s, f)

        match values.entry(hash, |v| key.equivalent(v.load().user.key()), |v| self.hash_builder.hash_one(v.load().user.key())) {
            hash_table::Entry::Occupied(occupied) => todo!(),
            hash_table::Entry::Vacant(vacant) => {

            },
        }
        // assert_eq!(
        //     strategy.start_insert(&value),
        //     Insert::Allow,
        //     "already purged"
        // );

        // let strategy_value = strategy.create_insert_value(&value);
        // match values.entry(
        //     hash,
        //     |v| v.load().user.key() == key,
        //     |v| self.hash_builder.hash_one(v.load().user.key()),
        // ) {
        //     hash_table::Entry::Occupied(occupied) => {
        //         let pointer = Pointer(Arc::new(Value {
        //             user: value,
        //             strategy: strategy_value,
        //         }));

        //         let prev = Pointer(occupied.get().swap(pointer.clone().0));
        //         strategy.remove(&prev);
        //         strategy.complete_insert(&pointer);
        //         pointer
        //     }
        //     hash_table::Entry::Vacant(vacant) => {
        //         let pointer = Pointer(Arc::new(Value {
        //             user: value,
        //             strategy: strategy_value,
        //         }));
        //         vacant.insert(pointer.clone().0.into());
        //         strategy.complete_insert(&pointer);
        //         pointer
        //     }
        // }
        todo!()
    }
}

pub struct OccupiedEntry<'a, T, Sv> {
    pointer: Pointer<T, Sv>,
    _marker: PhantomData<&'a ()>,
}

pub struct VacantEntry<'a, T, Sv> {
    hash: usize,
    _marker: PhantomData<&'a (T, Sv)>,
}

impl<'a, T, Sv> hash::OccupiedEntry<'a> for OccupiedEntry<'a, T, Sv>
where
    T: hash::Value + Send + Sync + 'static,
    Sv: Send + Sync + 'static,
{
    type Value = T;
    type Pointer = Pointer<T, Sv>;
    type VacantEntry = VacantEntry<'a, T, Sv>;

    fn get(&self) -> &T {
        &*self.pointer
    }

    fn pointer(&self) -> &Self::Pointer {
        &self.pointer
    }

    fn into_pointer(self) -> Self::Pointer {
        self.pointer
    }

    fn try_remove(self) -> Result<Self::Pointer, Entry<Self, Self::VacantEntry>> {
        todo!()
    }

    fn try_insert(self, value: T) -> Result<Self::Pointer, (T, Entry<Self, Self::VacantEntry>)> {
        todo!()
    }
}

impl<'a, T, Sv> hash::VacantEntry<'a> for VacantEntry<'a, T, Sv>
where
    T: hash::Value + Send + Sync + 'static,
    Sv: Send + Sync + 'static,
{
    type Value = T;
    type Pointer = Pointer<T, Sv>;
    type OccupiedEntry = OccupiedEntry<'a, T, Sv>;

    fn try_insert(
        self,
        value: Self::Value,
    ) -> Result<Self::Pointer, (Self::Value, Self::OccupiedEntry)> {
        todo!()
    }
}
