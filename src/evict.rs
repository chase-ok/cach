use std::ops::{Deref, DerefMut};

use parking_lot::{RwLockReadGuard, RwLockUpgradableReadGuard};


mod index;
pub mod lru;

pub trait Eviction<T: Clone> {
    type Value;
    type Shard;

    fn new_shard(&mut self, capacity: usize) -> Self::Shard;

    fn insert(
        &self,
        shard: &mut Self::Shard,
        construct: impl FnOnce(Self::Value) -> T,
    ) -> (T, Option<T>);

    fn touch(&self, shard: &Self::Shard, value: &Self::Value);
    // fn touch(&self, shard: impl UpgradeLock<Target = Self::Shard>, value: &Self::Value);

    fn remove(&self, shard: &mut Self::Shard, value: &Self::Value);

    fn replace(
        &self,
        shard: &mut Self::Shard,
        remove: &Self::Value,
        construct: impl FnOnce(Self::Value) -> T,
    ) -> T;
}

pub trait UpgradeLock: Deref {
    fn upgrade(self) -> impl Deref<Target = Self::Target> + DerefMut;
}

impl<T> UpgradeLock for RwLockUpgradableReadGuard<'_, T> {
    fn upgrade(self) -> impl Deref<Target = Self::Target> + DerefMut {
        RwLockUpgradableReadGuard::upgrade(self)
    }
}

impl<T> UpgradeLock for RwLockReadGuard<'_, T> {
    fn upgrade(self) -> impl Deref<Target = Self::Target> + DerefMut {
        let lock = RwLockReadGuard::rwlock(&self);
        drop(self);
        lock.write()
    }
}

#[derive(Debug)]
pub struct NoEviction;

impl<T: Clone> Eviction<T> for NoEviction {
    type Value = ();
    type Shard = ();

    fn new_shard(&mut self, _capacity: usize) -> Self::Shard {
        ()
    }

    fn insert(
        &self,
        _shard: &mut Self::Shard,
        construct: impl FnOnce(Self::Value) -> T,
    ) -> (T, Option<T>) {
        (construct(()), None)
    }

    fn touch(&self, _shard: &Self::Shard, _value: &Self::Value) {}

    fn remove(&self, _shard: &mut Self::Shard, _value: &Self::Value) {}

    fn replace(
        &self,
        _shard: &mut Self::Shard,
        _remove: &Self::Value,
        construct: impl FnOnce(Self::Value) -> T,
    ) -> T {
        construct(())
    }
}