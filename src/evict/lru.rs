use std::ops::Deref;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::time::{AtomicInstant, Clock, DefaultClock, TouchedTime};

use super::index::{IndexList, Key};
use super::{Eviction, UpgradeReadGuard};

#[derive(Debug)]
pub struct LruEviction;

impl<E: Deref + Clone> Eviction<E> for LruEviction {
    type State = Key;
    type Shard = IndexList<E>;

    fn new_shard(&mut self, capacity: usize) -> Self::Shard {
        IndexList::with_capacity(capacity)
    }

    fn insert(
        &self,
        shard: &mut Self::Shard,
        construct: impl FnOnce(Self::State) -> E,
    ) -> (E, Option<E>) {
        let removed = if shard.len() == shard.capacity() {
            shard.head_key().and_then(|k| shard.remove(k))
        } else {
            None
        };

        let (_key, value) = shard.insert_tail_with_key(construct);
        (value.clone(), removed)
    }

    fn touch(&self, shard: impl UpgradeReadGuard<Target = Self::Shard>, state: &Self::State, _entry: &E) {
        UpgradeReadGuard::upgrade(shard).move_to_tail(*state);
    }

    fn remove(&self, shard: &mut Self::Shard, state: &Self::State) {
        shard.remove(*state).unwrap();
    }

    fn replace(
        &self,
        shard: &mut Self::Shard,
        remove: &Self::State,
        construct: impl FnOnce(Self::State) -> E,
    ) -> E {
        shard.remove(*remove).unwrap();
        let (_key, value) = shard.insert_tail_with_key(construct);
        value.clone()
    }
}

#[derive(Debug)]
pub struct ApproximateLruEviction<C = DefaultClock> {
    clock: C,
    soft_window: Duration,
    hard_window: Duration,
}

impl<C: Default> ApproximateLruEviction<C> {
    pub fn new(soft_window: Duration) -> Self {
        Self::with_hard_window(soft_window, soft_window * 2)
    }

    pub fn with_hard_window(soft_window: Duration, hard_window: Duration) -> Self {
        Self::with_clock(soft_window, hard_window, C::default())
    }
}

impl<C> ApproximateLruEviction<C> {
    pub fn with_clock(soft_window: Duration, hard_window: Duration, clock: C) -> Self {
        Self {
            clock,
            soft_window,
            hard_window,
        }
    }
}

impl<C: Clock, E: Deref + Clone> Eviction<E> for ApproximateLruEviction<C> {
    type State = (AtomicInstant, <LruEviction as Eviction<E>>::State);
    type Shard = <LruEviction as Eviction<E>>::Shard;

    fn new_shard(&mut self, capacity: usize) -> Self::Shard {
        LruEviction.new_shard(capacity)
    }

    fn insert(
        &self,
        shard: &mut Self::Shard,
        construct: impl FnOnce(Self::State) -> E,
    ) -> (E, Option<E>) {
        LruEviction.insert(shard, |key| construct((self.clock.now().into(), key)))
    }

    fn touch(&self, shard: impl UpgradeReadGuard<Target = Self::Shard>, state: &Self::State, entry: &E) {
        let now = self.clock.now();
        let since_touched = now
            .checked_duration_since(state.0.load(Ordering::Relaxed))
            .unwrap_or_default();
        if since_touched >= self.hard_window {
            state.0.store(now, Ordering::Relaxed);
            LruEviction.touch(shard, &state.1, entry);
        } else if since_touched >= self.soft_window {
            if let Some(mut write) = UpgradeReadGuard::try_upgrade(shard) {
                state.0.store(now, Ordering::Relaxed);
                write.move_to_tail(state.1);
            }
        }
    }

    fn remove(&self, shard: &mut Self::Shard, value: &Self::State) {
        LruEviction.remove(shard, &value.1);
    }

    fn replace(
        &self,
        shard: &mut Self::Shard,
        remove: &Self::State,
        construct: impl FnOnce(Self::State) -> E,
    ) -> E {
        LruEviction.replace(shard, &remove.1, |key| {
            construct((self.clock.now().into(), key))
        })
    }
}

#[derive(Debug)]
pub struct IntrusiveApproximateLruEviction<C = DefaultClock> {
    clock: C,
    soft_window: Duration,
    hard_window: Duration,
}

impl<C: Default> IntrusiveApproximateLruEviction<C> {
    pub fn new(soft_window: Duration) -> Self {
        Self::with_hard_window(soft_window, soft_window * 2)
    }

    pub fn with_hard_window(soft_window: Duration, hard_window: Duration) -> Self {
        Self::with_clock(soft_window, hard_window, C::default())
    }
}

impl<C> IntrusiveApproximateLruEviction<C> {
    pub fn with_clock(soft_window: Duration, hard_window: Duration, clock: C) -> Self {
        Self {
            clock,
            soft_window,
            hard_window,
        }
    }
}

impl<C, E> Eviction<E> for IntrusiveApproximateLruEviction<C>
where 
    C: Clock,
    E: Deref + Clone,
    E::Target: TouchedTime
{
    type State = <LruEviction as Eviction<E>>::State;
    type Shard = <LruEviction as Eviction<E>>::Shard;

    fn new_shard(&mut self, capacity: usize) -> Self::Shard {
        LruEviction.new_shard(capacity)
    }

    fn insert(
        &self,
        shard: &mut Self::Shard,
        construct: impl FnOnce(Self::State) -> E,
    ) -> (E, Option<E>) {
        LruEviction.insert(shard, construct)
    }

    fn touch(&self, shard: impl UpgradeReadGuard<Target = Self::Shard>, state: &Self::State, entry: &E) {
        let now = self.clock.now();
        let since_touched = now.checked_duration_since(entry.last_touched()).unwrap_or_default();

        if since_touched >= self.hard_window {
            entry.touch(now);
            LruEviction.touch(shard, state, entry);
        } else if since_touched >= self.soft_window {
            if let Some(mut write) = UpgradeReadGuard::try_upgrade(shard) {
                entry.touch(now);
                write.move_to_tail(*state);
            }
        }
    }

    fn remove(&self, shard: &mut Self::Shard, value: &Self::State) {
        LruEviction.remove(shard, value);
    }

    fn replace(
        &self,
        shard: &mut Self::Shard,
        remove: &Self::State,
        construct: impl FnOnce(Self::State) -> E,
    ) -> E {
        LruEviction.replace(shard, remove, construct)
    }
}