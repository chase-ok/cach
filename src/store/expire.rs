use std::time::{Duration, Instant};

use super::{
    strategy::{BuildStrategy, Strategy, StrategyPointer, Lock, ReadExclusive, ReadShared}, Pointer
};

pub trait Expire<P: Pointer> {
    type Value: 'static;

    fn insert(&self, target: &P::Target) -> Self::Value;

    fn is_expired(&self, pointer: &P, value: &Self::Value) -> bool;
}

pub struct ExpireLayer<E>(E);

impl<P, E> Strategy<P> for ExpireLayer<E>
where
    P: StrategyPointer<StrategyValue = E::Value>,
    E: Expire<P>,
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
        if self.0.is_expired(pointer, pointer.strategy_value()) {
            ReadShared::Remove
        } else {
            ReadShared::Allow
        }
    }

    fn read(&mut self, pointer: &P) -> ReadExclusive {
        if self.0.is_expired(pointer, pointer.strategy_value()) {
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

    fn is_expired(&self, _pointer: &P, value: &Self::Value) -> bool {
        self.now.now() >= *value
    }
}

impl<T, N: Now> BuildStrategy<T> for ExpireAfterWrite<N> {
    type Value = Instant;

    type Strategy<P>
        = ExpireLayer<Self>
    where
        P: StrategyPointer<Target = T, StrategyValue = Self::Value>;


    fn build_sharded<P>(self, shards: usize) -> impl Iterator<Item = Self::Strategy<P>>
    where
        P: StrategyPointer<Target = T, StrategyValue = Self::Value> {
        std::iter::repeat_with(move || ExpireLayer(self.clone())).take(shards)
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

    fn is_expired(&self, pointer: &P, _value: &()) -> bool {
        self.now.now() >= pointer.expire_at()
    }
}

impl<T: ExpireAt, N: Now> BuildStrategy<T> for ExpireAtIntrusive<N> {
    type Value = ();

    type Strategy<P>
        = ExpireLayer<Self>
    where
        P: StrategyPointer<Target = T, StrategyValue = Self::Value>;


    fn build_sharded<P>(self, shards: usize) -> impl Iterator<Item = Self::Strategy<P>>
    where
        P: StrategyPointer<Target = T, StrategyValue = Self::Value> {
        std::iter::repeat_with(move || ExpireLayer(self.clone())).take(shards)
    }
}

