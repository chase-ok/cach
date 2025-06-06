use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use super::{
    Pointer,
    strategy::{BuildStrategy, Lock, ReadExclusive, ReadShared, Strategy, StrategyPointer},
};

pub trait Expire<P: Pointer> {
    type Value: 'static;

    fn insert(&self, target: &P::Target) -> Self::Value;

    // XX must be const
    fn expires_at(&self, pointer: &P, value: &Self::Value) -> Instant;
}

pub struct ExpireLayer<E, N = SystemInstant>(E, N);

impl<P, E, N> Strategy<P> for ExpireLayer<E, N>
where
    P: StrategyPointer<StrategyValue = E::Value>,
    E: Expire<P>,
    N: Now,
{
    type Value = E::Value;

    const READ_LOCK: Lock = Lock::Shared;
    const REMOVE_LOCK: Lock = Lock::Shared;
    const INSERT_LOCK: Lock = Lock::Shared;
    const PURGE_LOCK: Lock = Lock::Shared;

    fn create_insert_shared_value(&self, target: &P::Target) -> Self::Value {
        self.0.insert(target)
    }

    fn create_insert_value(&mut self, target: &P::Target) -> Self::Value {
        self.0.insert(target)
    }

    #[inline]
    fn read_shared(&self, pointer: &P) -> ReadShared {
        if self.1.now() >= self.0.expires_at(pointer, pointer.strategy_value()) {
            ReadShared::Remove
        } else {
            ReadShared::Allow
        }
    }

    fn read(&mut self, pointer: &P) -> ReadExclusive {
        if self.1.now() >= self.0.expires_at(pointer, pointer.strategy_value()) {
            ReadExclusive::Remove
        } else {
            ReadExclusive::Allow
        }
    }
}

pub trait Now: Clone {
    fn now(&self) -> Instant;
}

#[derive(Debug, Clone, Copy)]
pub struct SystemInstant;

impl Now for SystemInstant {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ExpireAfterWrite<N = SystemInstant> {
    now: N,
    duration: Duration,
}

impl ExpireAfterWrite {
    pub fn new(duration: Duration) -> Self {
        Self {
            now: SystemInstant,
            duration,
        }
    }
}

impl<P: Pointer, N: Now> Expire<P> for ExpireAfterWrite<N> {
    type Value = Instant;

    fn insert(&self, _target: &P::Target) -> Self::Value {
        self.now.now() + self.duration
    }

    fn expires_at(&self, _pointer: &P, value: &Self::Value) -> Instant {
        *value
    }
}

impl<T, N: Now> BuildStrategy<T> for ExpireAfterWrite<N> {
    type Value = Instant;

    type Strategy<P>
        = ExpireLayer<Self, N>
    where
        P: StrategyPointer<Target = T, StrategyValue = Self::Value>;

    fn build_sharded<P>(self, shards: usize) -> impl Iterator<Item = Self::Strategy<P>>
    where
        P: StrategyPointer<Target = T, StrategyValue = Self::Value>,
    {
        std::iter::repeat_with(move || ExpireLayer(self.clone(), self.now.clone())).take(shards)
    }
}

#[derive(Default, Debug, Clone, Copy)]
pub struct ExpireAtIntrusive<N = SystemInstant> {
    now: N,
}

impl ExpireAtIntrusive {
    pub const fn new() -> Self {
        Self { now: SystemInstant }
    }
}

pub trait ExpireAt {
    fn expire_at(&self) -> Instant;
}

impl<P: Pointer, N: Now> Expire<P> for ExpireAtIntrusive<N>
where
    P::Target: ExpireAt,
{
    type Value = ();

    fn insert(&self, _target: &P::Target) -> Self::Value {
        ()
    }

    fn expires_at(&self, pointer: &P, _value: &Self::Value) -> Instant {
        // XX make const?
        pointer.expire_at()
    }
}

impl<T: ExpireAt, N: Now> BuildStrategy<T> for ExpireAtIntrusive<N> {
    type Value = ();

    type Strategy<P>
        = ExpireLayer<Self, N>
    where
        P: StrategyPointer<Target = T, StrategyValue = Self::Value>;

    fn build_sharded<P>(self, shards: usize) -> impl Iterator<Item = Self::Strategy<P>>
    where
        P: StrategyPointer<Target = T, StrategyValue = Self::Value>,
    {
        std::iter::repeat_with(move || ExpireLayer(self.clone(), self.now.clone())).take(shards)
    }
}

pub struct ExpireFrozenLayer<P, N = SystemInstant> {
    now: N,
    sorted: BTreeMap<Instant, P>,
}

impl<P, N> Strategy<P> for ExpireFrozenLayer<P, N>
where
    P: StrategyPointer<StrategyValue = Instant>,
    P::Target: ExpireAt,
    N: Now,
{
    type Value = Instant;

    const READ_LOCK: Lock = Lock::Shared;
    const REMOVE_LOCK: Lock = Lock::Exclusive;
    const INSERT_LOCK: Lock = Lock::Exclusive;
    const PURGE_LOCK: Lock = Lock::Exclusive;

    fn create_insert_shared_value(&self, target: &P::Target) -> Self::Value {
        target.expire_at()
    }

    fn create_insert_value(&mut self, target: &P::Target) -> Self::Value {
        target.expire_at()
    }

    #[inline]
    fn read_shared(&self, pointer: &P) -> ReadShared {
        if self.now.now() >= *pointer.strategy_value() {
            ReadShared::Remove
        } else {
            ReadShared::Allow
        }
    }

    fn read(&mut self, pointer: &P) -> ReadExclusive {
        if self.now.now() >= *pointer.strategy_value() {
            ReadExclusive::Remove
        } else {
            ReadExclusive::Allow
        }
    }

    fn purge(&mut self) -> impl Iterator<Item = P> {
        let now = self.now.now();
        std::iter::from_fn(move || {
            self.sorted
                .first_entry()
                .filter(|e| *e.key() <= now)
                .map(|e| e.remove())
        })
        .take(16)
    }
}
