// use super::BuildStore;

use super::Pointer;

mod and_then;
pub use and_then::{AndThen, AndThenStrategy};
use stable_deref_trait::StableDeref;

mod buffered;

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
        AndThen::new(self, next)
    }
}

// XX make hidden
pub trait StrategyPointer: Pointer {
    type StrategyValue: ?Sized;

    fn strategy_value(&self) -> &Self::StrategyValue;
}

pub trait Strategy<P: StrategyPointer<StrategyValue = Self::Value>> {
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
    fn start_remove_shared(&self, _pointer: &P) -> RemoveShared {
        RemoveShared::Allow
    }

    #[inline]
    fn complete_remove_shared(&self, _pointer: &P) {}

    #[inline]
    fn fail_remove_shared(&self, _pointer: &P) {}

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
