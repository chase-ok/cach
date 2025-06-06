
use std::{marker::PhantomData, ops::Deref};

use ref_cast::RefCast;
use smallvec::SmallVec;
use stable_deref_trait::{CloneStableDeref, StableDeref};

use super::{BuildStrategy, Insert, InsertShared, Lock, PurgeShared, ReadExclusive, ReadShared, RemoveShared, Strategy, StrategyPointer};

#[derive(Debug, Clone, Copy)]
pub struct AndThen<S0, S1> {
    s0: S0,
    s1: S1,
}
impl<S0, S1> AndThen<S0, S1> {
    pub fn new(s0: S0, s1: S1) -> Self {
        Self { s0, s1 }
    }
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
    fn start_remove_shared(&self, pointer: &P) -> RemoveShared {
        match self.s0.start_remove_shared(StrategyPointer0::ref_cast(pointer)) {
            RemoveShared::Allow => self.s1.start_remove_shared(StrategyPointer1::ref_cast(pointer)),
            r => r,
        }
    }

    #[inline]
    fn complete_remove_shared(&self, pointer: &P) {
        self.s0.complete_remove_shared(StrategyPointer0::ref_cast(pointer));
        self.s1.complete_remove_shared(StrategyPointer1::ref_cast(pointer));
    }

    fn fail_remove_shared(&self, pointer: &P) {
        self.s0.fail_remove_shared(StrategyPointer0::ref_cast(pointer));
        self.s1.fail_remove_shared(StrategyPointer1::ref_cast(pointer));
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

// XX
unsafe impl<P: StableDeref, V0: 'static, V1: 'static> StableDeref for StrategyPointer0<P, V0, V1> { }
unsafe impl<P: CloneStableDeref, V0: 'static, V1: 'static> CloneStableDeref for StrategyPointer0<P, V0, V1> { }

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

// XX
unsafe impl<P: StableDeref, V0: 'static, V1: 'static> StableDeref for StrategyPointer1<P, V0, V1> { }
unsafe impl<P: CloneStableDeref, V0: 'static, V1: 'static> CloneStableDeref for StrategyPointer1<P, V0, V1> { }

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