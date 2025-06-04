// use super::BuildStore;

use std::{marker::PhantomData, ops::Deref};

use ref_cast::RefCast;
use smallvec::SmallVec;

use super::Pointer;

pub trait BuildStrategy<T> {
    type Value: 'static + Send + Sync;

    type Strategy<P>: Strategy<P, Value = Self::Value>
    where
        P: StrategyPointer<Target = T, StrategyValue = Self::Value>;

    fn build_sharded<P>(self, shards: usize) -> impl Iterator<Item = Self::Strategy<P>>
    where
        P: StrategyPointer<Target = T, StrategyValue = Self::Value>;

    fn build<P>(self) -> Self::Strategy<P>
    where
        Self: Sized,
        P: StrategyPointer<Target = T, StrategyValue = Self::Value>,
    {
        self.build_sharded::<P>(1).next().unwrap()
    }

    fn and_then<N>(self, next: N) -> AndThen<Self, N>
    where
        Self: Sized,
    {
        AndThen { s0: self, s1: next }
    }
}

// XX make hidden
pub trait StrategyPointer: Pointer {
    type StrategyValue: ?Sized;

    fn strategy_value(&self) -> &Self::StrategyValue;
}

pub trait Strategy<P: Pointer + StrategyPointer<StrategyValue = Self::Value>> {
    type Value: 'static;

    const READ_LOCK: Lock;

    #[inline]
    fn read_shared(&self, _pointer: &P) -> ReadShared {
        ReadShared::Allow
    }

    #[inline]
    fn read(&mut self, _pointer: &P) -> ReadExclusive {
        ReadExclusive::Allow
    }

    const REMOVE_LOCK: Lock;

    #[inline]
    fn remove_shared(&self, _pointer: &P) -> RemoveShared {
        RemoveShared::Allow
    }

    #[inline]
    fn remove(&mut self, _pointer: &P) {}

    const INSERT_LOCK: Lock;

    #[inline]
    fn start_insert_shared(&self, _target: &P::Target) -> InsertShared {
        InsertShared::Allow
    }

    fn create_insert_shared_value(&self, target: &P::Target) -> Self::Value;

    #[inline]
    fn fail_insert_shared(&self, _target: &P::Target, _value: Self::Value) {}

    #[inline]
    fn complete_insert_shared(&self, _pointer: &P) {}

    #[inline]
    fn start_insert(&mut self, _target: &P::Target) -> Insert {
        Insert::Allow
    }

    fn create_insert_value(&mut self, target: &P::Target) -> Self::Value;

    #[inline]
    fn complete_insert(&mut self, _pointer: &P) {}

    const PURGE_LOCK: Lock;

    #[inline]
    fn start_purge_shared(&self) -> PurgeShared<impl Iterator<Item = P>> {
        PurgeShared::Purge(std::iter::empty())
    }

    #[inline]
    fn complete_purge_shared(&self, _pointer: &P) {}

    #[inline]
    fn purge(&mut self) -> impl Iterator<Item = P> {
        std::iter::empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lock {
    Shared,
    Exclusive,
}

impl Lock {
    pub const fn and(self, rhs: Self) -> Self {
        match (self, rhs) {
            (_, Lock::Exclusive) | (Lock::Exclusive, _) => Lock::Exclusive,
            _ => Lock::Shared,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadShared {
    Allow,
    Remove,
    RequireExclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadExclusive {
    Allow,
    Remove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveShared {
    Allow,
    RequireExclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertShared {
    Allow,
    RequirePurge,
    RequireExclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Insert {
    Allow,
    RequirePurge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PurgeShared<I> {
    Purge(I),
    RequireExclusive,
}

#[derive(Debug, Clone, Copy)]
pub struct AndThen<L0, L1> {
    s0: L0,
    s1: L1,
}

impl<T, S0, S1> BuildStrategy<T> for AndThen<S0, S1>
where
    S0: BuildStrategy<T>,
    S1: BuildStrategy<T>,
{
    type Value = (S0::Value, S1::Value);

    type Strategy<P>
        = AndThenStrategy<
        S0::Strategy<StrategyPointer0<P, S0::Value, S1::Value>>,
        S1::Strategy<StrategyPointer1<P, S0::Value, S1::Value>>,
        S0::Value,
        S1::Value,
    >
    where
        P: StrategyPointer<Target = T, StrategyValue = Self::Value>;

    fn build_sharded<P>(self, shards: usize) -> impl Iterator<Item = Self::Strategy<P>>
    where
        P: StrategyPointer<Target = T, StrategyValue = Self::Value>,
    {
        self.s0
            .build_sharded(shards)
            .zip(self.s1.build_sharded(shards))
            .map(|(s0, s1)| AndThenStrategy {
                s0,
                s1,
                _marker: PhantomData,
            })
    }
}

pub struct AndThenStrategy<S0, S1, V0, V1> {
    s0: S0,
    s1: S1,
    _marker: PhantomData<(V0, V1)>,
}

impl<P, S0, S1, V0, V1> Strategy<P> for AndThenStrategy<S0, S1, V0, V1>
where
    P: StrategyPointer<StrategyValue = (V0, V1)>,
    S0: Strategy<StrategyPointer0<P, V0, V1>, Value = V0>,
    S1: Strategy<StrategyPointer1<P, V0, V1>, Value = V1>,
    V0: 'static,
    V1: 'static,
{
    type Value = (V0, V1);

    #[inline]
    fn create_insert_shared_value(&self, target: &P::Target) -> Self::Value {
        (
            self.s0.create_insert_shared_value(target),
            self.s1.create_insert_shared_value(target),
        )
    }

    #[inline]
    fn create_insert_value(&mut self, target: &<P>::Target) -> Self::Value {
        (
            self.s0.create_insert_value(target),
            self.s1.create_insert_value(target),
        )
    }

    const READ_LOCK: Lock = S0::READ_LOCK.and(S1::READ_LOCK);

    #[inline]
    fn read_shared(&self, pointer: &P) -> ReadShared {
        match self.s0.read_shared(StrategyPointer0::ref_cast(pointer)) {
            ReadShared::Allow => self.s1.read_shared(StrategyPointer1::ref_cast(pointer)),
            r => r,
        }
    }

    #[inline]
    fn read(&mut self, pointer: &P) -> ReadExclusive {
        match self.s0.read(StrategyPointer0::ref_cast(pointer)) {
            ReadExclusive::Allow => self.s1.read(StrategyPointer1::ref_cast(pointer)),
            r => r,
        }
    }

    const REMOVE_LOCK: Lock = S0::REMOVE_LOCK.and(S1::REMOVE_LOCK);

    #[inline]
    fn remove_shared(&self, pointer: &P) -> RemoveShared {
        match self.s0.remove_shared(StrategyPointer0::ref_cast(pointer)) {
            RemoveShared::Allow => self.s1.remove_shared(StrategyPointer1::ref_cast(pointer)),
            r => r,
        }
    }

    #[inline]
    fn remove(&mut self, pointer: &P) {
        self.s0.remove(StrategyPointer0::ref_cast(pointer));
        self.s1.remove(StrategyPointer1::ref_cast(pointer));
    }

    const INSERT_LOCK: Lock = S0::INSERT_LOCK.and(S1::INSERT_LOCK);

    #[inline]
    fn start_insert_shared(&self, target: &P::Target) -> InsertShared {
        match self.s0.start_insert_shared(target) {
            InsertShared::Allow => self.s1.start_insert_shared(target),
            i => i,
        }
    }

    #[inline]
    fn fail_insert_shared(&self, target: &P::Target, value: Self::Value) {
        self.s0.fail_insert_shared(target, value.0);
        self.s1.fail_insert_shared(target, value.1);
    }

    #[inline]
    fn complete_insert_shared(&self, pointer: &P) {
        self.s0
            .complete_insert_shared(StrategyPointer0::ref_cast(pointer));
        self.s1
            .complete_insert_shared(StrategyPointer1::ref_cast(pointer));
    }

    #[inline]
    fn start_insert(&mut self, target: &P::Target) -> Insert {
        match self.s0.start_insert(target) {
            Insert::Allow => self.s1.start_insert(target),
            i => i,
        }
    }

    #[inline]
    fn complete_insert(&mut self, pointer: &P) {
        self.s0.complete_insert(StrategyPointer0::ref_cast(pointer));
        self.s1.complete_insert(StrategyPointer1::ref_cast(pointer));
    }

    const PURGE_LOCK: Lock = S0::PURGE_LOCK.and(S1::PURGE_LOCK);

    #[inline]
    fn start_purge_shared(&self) -> PurgeShared<impl Iterator<Item = P>> {
        match self.s0.start_purge_shared() {
            PurgeShared::Purge(i0) => match self.s1.start_purge_shared() {
                PurgeShared::Purge(i1) => {
                    PurgeShared::Purge(i0.map(|p| p.pointer).chain(i1.map(|p| p.pointer)))
                }
                PurgeShared::RequireExclusive => PurgeShared::RequireExclusive,
            },
            PurgeShared::RequireExclusive => PurgeShared::RequireExclusive,
        }
    }

    #[inline]
    fn complete_purge_shared(&self, pointer: &P) {
        self.s0
            .complete_purge_shared(StrategyPointer0::ref_cast(pointer));
        self.s1
            .complete_purge_shared(StrategyPointer1::ref_cast(pointer));
    }

    #[inline]
    fn purge(&mut self) -> impl Iterator<Item = P> {
        // XX we could write our own generator here instead of the small vec alloc?
        let mut pointers = SmallVec::<[P; 4]>::new();
        pointers.extend(self.s0.purge().map(|p| p.pointer));
        for p in &pointers {
            self.s1.remove(StrategyPointer1::ref_cast(p));
        }

        let p1_start = pointers.len();
        pointers.extend(self.s1.purge().map(|p| p.pointer));
        for p in &pointers[p1_start..] {
            self.s0.remove(StrategyPointer0::ref_cast(p))
        }

        pointers.into_iter()
    }
}

#[derive(RefCast)]
#[repr(transparent)]
#[doc(hidden)]
pub struct StrategyPointer0<P, V0, V1> {
    pointer: P,
    _marker: PhantomData<(V0, V1)>,
}

impl<P: Clone, V0, V1> Clone for StrategyPointer0<P, V0, V1> {
    fn clone(&self) -> Self {
        Self {
            pointer: self.pointer.clone(),
            _marker: PhantomData,
        }
    }
}

impl<P, V0: 'static, V1: 'static> StrategyPointer for StrategyPointer0<P, V0, V1>
where
    P: StrategyPointer<StrategyValue = (V0, V1)>,
{
    type StrategyValue = V0;

    fn strategy_value(&self) -> &V0 {
        &self.pointer.strategy_value().0
    }
}

impl<P, V0: 'static, V1: 'static> Deref for StrategyPointer0<P, V0, V1>
where
    P: Deref,
{
    type Target = P::Target;

    fn deref(&self) -> &Self::Target {
        &*self.pointer
    }
}

#[derive(RefCast)]
#[repr(transparent)]
#[doc(hidden)]
pub struct StrategyPointer1<P, V0, V1> {
    pointer: P,
    _marker: PhantomData<(V0, V1)>,
}

impl<P: Clone, V0, V1> Clone for StrategyPointer1<P, V0, V1> {
    fn clone(&self) -> Self {
        Self {
            pointer: self.pointer.clone(),
            _marker: PhantomData,
        }
    }
}

impl<P, V0: 'static, V1: 'static> Deref for StrategyPointer1<P, V0, V1>
where
    P: Deref,
{
    type Target = P::Target;

    fn deref(&self) -> &Self::Target {
        &*self.pointer
    }
}

impl<P, V0: 'static, V1: 'static> StrategyPointer for StrategyPointer1<P, V0, V1>
where
    P: StrategyPointer<StrategyValue = (V0, V1)>,
{
    type StrategyValue = V1;
    fn strategy_value(&self) -> &V1 {
        &self.pointer.strategy_value().1
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NoStrategy;

impl<T> BuildStrategy<T> for NoStrategy {
    type Value = ();

    type Strategy<P>
        = Self
    where
        P: StrategyPointer<Target = T, StrategyValue = Self::Value>;

    fn build_sharded<P>(self, shards: usize) -> impl Iterator<Item = Self::Strategy<P>>
    where
        P: StrategyPointer<Target = T, StrategyValue = Self::Value>,
    {
        std::iter::repeat_n(self, shards)
    }
}

impl<P> Strategy<P> for NoStrategy
where
    P: StrategyPointer<StrategyValue = ()>,
{
    type Value = ();

    const READ_LOCK: Lock = Lock::Shared;
    const REMOVE_LOCK: Lock = Lock::Shared;
    const INSERT_LOCK: Lock = Lock::Shared;
    const PURGE_LOCK: Lock = Lock::Shared;

    #[inline]
    fn create_insert_shared_value(&self, _target: &P::Target) -> Self::Value {
        ()
    }

    #[inline]
    fn create_insert_value(&mut self, _target: &P::Target) -> Self::Value {
        ()
    }
}
