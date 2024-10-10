use std::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

use parking_lot::{RwLockReadGuard, RwLockUpgradableReadGuard, RwLockWriteGuard};

mod index;
pub mod lru;

pub trait Eviction<P> {
    type Value;
    type Shard;

    fn new_shard(&mut self, capacity: usize) -> Self::Shard;

    fn insert(
        &self,
        shard: &mut Self::Shard,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, Option<P>);

    fn touch(
        &self,
        shard: impl UpgradeReadGuard<Target = Self::Shard>,
        value: &Self::Value,
        pointer: &P,
    );

    fn remove(&self, shard: &mut Self::Shard, value: &Self::Value);

    fn replace(
        &self,
        shard: &mut Self::Shard,
        state: &Self::Value,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> P;
}

pub trait UpgradeReadGuard: Deref {
    fn upgrade(self) -> impl Deref<Target = Self::Target> + DerefMut;

    fn try_upgrade(self) -> Option<impl Deref<Target = Self::Target> + DerefMut>
    where
        Self: Sized,
    {
        Some(self.upgrade())
    }
}

impl<T> UpgradeReadGuard for RwLockUpgradableReadGuard<'_, T> {
    fn upgrade(self) -> impl Deref<Target = Self::Target> + DerefMut {
        RwLockUpgradableReadGuard::upgrade(self)
    }

    fn try_upgrade(self) -> Option<impl Deref<Target = Self::Target> + DerefMut> {
        RwLockUpgradableReadGuard::try_upgrade(self).ok()
    }
}

impl<T> UpgradeReadGuard for RwLockReadGuard<'_, T> {
    fn upgrade(self) -> impl Deref<Target = Self::Target> + DerefMut {
        let lock = RwLockReadGuard::rwlock(&self);
        drop(self);
        lock.write()
    }

    fn try_upgrade(self) -> Option<impl Deref<Target = Self::Target> + DerefMut> {
        let lock = RwLockReadGuard::rwlock(&self);
        drop(self);
        lock.try_write()
    }
}

impl<T> UpgradeReadGuard for RwLockWriteGuard<'_, T> {
    fn upgrade(self) -> impl Deref<Target = Self::Target> + DerefMut {
        self
    }
}

pub struct MapUpgradeReadGuard<G, T, D, DM> {
    guard: G,
    _target: PhantomData<T>,
    deref: D,
    deref_mut: DM,
}

impl<G, T, D, DM> MapUpgradeReadGuard<G, T, D, DM>
where
    G: Deref,
    D: Fn(&G::Target) -> &T,
    DM: Fn(&mut G::Target) -> &mut T,
{
    pub fn new(guard: G, deref: D, deref_mut: DM) -> Self {
        Self {
            guard,
            _target: PhantomData,
            deref,
            deref_mut,
        }
    }

    pub fn guard(this: &Self) -> &G {
        &this.guard
    }

    pub fn guard_mut(this: &mut Self) -> &mut G {
        &mut this.guard
    }

    pub fn into_guard(this: Self) -> G {
        this.guard
    }
}

impl<G, T, D, DM> Deref for MapUpgradeReadGuard<G, T, D, DM>
where
    G: Deref,
    D: Fn(&G::Target) -> &T,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        (self.deref)(&self.guard)
    }
}

impl<G, T, D, DM> DerefMut for MapUpgradeReadGuard<G, T, D, DM>
where
    G: DerefMut,
    D: Fn(&G::Target) -> &T,
    DM: Fn(&mut G::Target) -> &mut T,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        (self.deref_mut)(&mut self.guard)
    }
}

impl<G, T, D, DM> UpgradeReadGuard for MapUpgradeReadGuard<G, T, D, DM>
where
    G: UpgradeReadGuard,
    D: Fn(&G::Target) -> &T,
    DM: Fn(&mut G::Target) -> &mut T,
{
    fn upgrade(self) -> impl Deref<Target = Self::Target> + DerefMut {
        MapUpgradeReadGuard {
            guard: self.guard.upgrade(),
            _target: PhantomData,
            deref: self.deref,
            deref_mut: self.deref_mut,
        }
    }
}

#[derive(Debug, Default)]
pub struct NoEviction;

impl<E> Eviction<E> for NoEviction {
    type Value = ();
    type Shard = ();

    fn new_shard(&mut self, _capacity: usize) -> Self::Shard {
        ()
    }

    fn insert(
        &self,
        _shard: &mut Self::Shard,
        construct: impl FnOnce(Self::Value) -> E,
    ) -> (E, Option<E>) {
        (construct(()), None)
    }

    fn touch(
        &self,
        _shard: impl UpgradeReadGuard<Target = Self::Shard>,
        _state: &Self::Value,
        _entry: &E,
    ) { }

    fn remove(&self, _shard: &mut Self::Shard, _state: &Self::Value) { }

    fn replace(
        &self,
        _shard: &mut Self::Shard,
        _state: &Self::Value,
        construct: impl FnOnce(Self::Value) -> E,
    ) -> E {
        construct(())
    }
}