use std::{marker::PhantomData, ops::{Deref, DerefMut}};

use parking_lot::{RwLockReadGuard, RwLockUpgradableReadGuard, RwLockWriteGuard};


pub trait UpgradeReadGuard: Deref {
    type WriteGuard: Deref<Target = Self::Target> + DerefMut;

    fn upgrade(self) -> Self::WriteGuard;

    // XX: remove if not needed?
    fn try_upgrade(self) -> Option<Self::WriteGuard>
    where
        Self: Sized,
    {
        Some(self.upgrade())
    }
}

impl<'a, T> UpgradeReadGuard for RwLockUpgradableReadGuard<'a, T> {
    type WriteGuard = RwLockWriteGuard<'a, T>;

    fn upgrade(self) -> Self::WriteGuard {
        RwLockUpgradableReadGuard::upgrade(self)
    }

    fn try_upgrade(self) -> Option<Self::WriteGuard> {
        RwLockUpgradableReadGuard::try_upgrade(self).ok()
    }
}

impl<'a, T> UpgradeReadGuard for RwLockReadGuard<'a, T> {
    type WriteGuard = RwLockWriteGuard<'a, T>;

    fn upgrade(self) -> Self::WriteGuard {
        let lock = RwLockReadGuard::rwlock(&self);
        drop(self);
        lock.write()
    }

    fn try_upgrade(self) -> Option<Self::WriteGuard> {
        let lock = RwLockReadGuard::rwlock(&self);
        drop(self);
        lock.try_write()
    }
}

impl<T> UpgradeReadGuard for RwLockWriteGuard<'_, T> {
    type WriteGuard = Self;

    fn upgrade(self) -> Self {
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
    type WriteGuard = MapUpgradeReadGuard<G::WriteGuard, T, D, DM>;

    fn upgrade(self) -> Self::WriteGuard {
        MapUpgradeReadGuard {
            guard: self.guard.upgrade(),
            _target: PhantomData,
            deref: self.deref,
            deref_mut: self.deref_mut,
        }
    }
}

impl<T> UpgradeReadGuard for &mut T {
    type WriteGuard = Self;

    fn upgrade(self) -> Self::WriteGuard {
        self
    }
}

pub(crate) struct UpgradeReadGuardCell<G: UpgradeReadGuard>(UpgradeReadGuardCellState<G>);

enum UpgradeReadGuardCellState<G: UpgradeReadGuard> {
    None,
    Read(G),
    Write(G::WriteGuard)
}

impl<G: UpgradeReadGuard> UpgradeReadGuardCell<G> {
    pub fn new(guard: G) -> Self {
        Self(UpgradeReadGuardCellState::Read(guard))
    }

    pub fn borrow(&mut self) -> UpgradeReadGuardCellRef<'_, G> {
        UpgradeReadGuardCellRef(&mut self.0)
    }
}

pub struct UpgradeReadGuardCellRef<'a, G: UpgradeReadGuard>(&'a mut UpgradeReadGuardCellState<G>);

pub struct UpgradeReadGuardCellRefMut<'a, G: UpgradeReadGuard>(&'a mut UpgradeReadGuardCellState<G>);

impl<G: UpgradeReadGuard> Deref for UpgradeReadGuardCellRef<'_, G> {
    type Target = G::Target;

    fn deref(&self) -> &Self::Target {
        match &self.0 {
            UpgradeReadGuardCellState::None | UpgradeReadGuardCellState::Write(_) => unreachable!(),
            UpgradeReadGuardCellState::Read(r) => &r,
        }
    }
}

impl<G: UpgradeReadGuard> Deref for UpgradeReadGuardCellRefMut<'_, G> {
    type Target = G::Target;

    fn deref(&self) -> &Self::Target {
        match &self.0 {
            UpgradeReadGuardCellState::None | UpgradeReadGuardCellState::Read(_) => unreachable!(),
            UpgradeReadGuardCellState::Write(w) => &w,
        }
    }
}

impl<G: UpgradeReadGuard> DerefMut for UpgradeReadGuardCellRefMut<'_, G> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match &mut self.0 {
            UpgradeReadGuardCellState::None | UpgradeReadGuardCellState::Read(_) => unreachable!(),
            UpgradeReadGuardCellState::Write(w) => w,
        }
    }
}

impl<'a, G: UpgradeReadGuard> UpgradeReadGuard for UpgradeReadGuardCellRef<'a, G> {
    type WriteGuard = UpgradeReadGuardCellRefMut<'a, G>;

    fn upgrade(self) -> Self::WriteGuard {
        let UpgradeReadGuardCellState::Read(guard) = std::mem::replace(self.0, UpgradeReadGuardCellState::None) else {
            unreachable!()
        };
        *self.0 = UpgradeReadGuardCellState::Write(guard.upgrade());
        UpgradeReadGuardCellRefMut(self.0)
    }
}
