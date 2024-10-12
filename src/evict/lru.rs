use std::ops::Deref;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::time::{AtomicInstant, Clock, DefaultClock, TouchedTime};

use super::index::{IndexList, Key};
use super::{Eviction, UpgradeReadGuard};

#[derive(Debug)]
pub struct EvictLeastRecentlyUsed;

impl<P: Clone> Eviction<P> for EvictLeastRecentlyUsed {
    type Value = Key;
    type Queue = IndexList<P>;

    fn new_queue(&mut self, capacity: usize) -> Self::Queue {
        IndexList::with_capacity(capacity)
    }

    fn insert(
        &self,
        shard: &mut Self::Queue,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>) {
        let removed = if shard.len() == shard.capacity() {
            shard.head_key().and_then(|k| shard.remove(k))
        } else {
            None
        };

        let (_key, value) = shard.insert_tail_with_key(construct);
        (value.clone(), removed.into_iter())
    }

    fn touch(&self, queue: impl UpgradeReadGuard<Target = Self::Queue>, state: &Self::Value, _entry: &P) {
        UpgradeReadGuard::upgrade(queue).move_to_tail(*state);
    }

    fn remove(&self, queue: &mut Self::Queue, state: &Self::Value) {
        queue.remove(*state).unwrap();
    }

    fn replace(
        &self,
        queue: &mut Self::Queue,
        remove: &Self::Value,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>) {
        queue.remove(*remove).unwrap();
        let (_key, value) = queue.insert_tail_with_key(construct);
        (value.clone(), std::iter::empty())
    }
}

#[derive(Debug)]
pub struct ApproxLeastRecentlyUsedEviction<C = DefaultClock> {
    clock: C,
    soft_window: Duration,
    hard_window: Duration,
}

impl<C: Default> ApproxLeastRecentlyUsedEviction<C> {
    pub fn new(soft_window: Duration) -> Self {
        Self::with_hard_window(soft_window, soft_window * 2)
    }

    pub fn with_hard_window(soft_window: Duration, hard_window: Duration) -> Self {
        Self::with_clock(soft_window, hard_window, C::default())
    }
}

impl<C> ApproxLeastRecentlyUsedEviction<C> {
    pub fn with_clock(soft_window: Duration, hard_window: Duration, clock: C) -> Self {
        Self {
            clock,
            soft_window,
            hard_window,
        }
    }
}

impl<C: Clock, P: Clone> Eviction<P> for ApproxLeastRecentlyUsedEviction<C> {
    type Value = (AtomicInstant, <EvictLeastRecentlyUsed as Eviction<P>>::Value);
    type Queue = <EvictLeastRecentlyUsed as Eviction<P>>::Queue;

    fn new_queue(&mut self, capacity: usize) -> Self::Queue {
        EvictLeastRecentlyUsed.new_queue(capacity)
    }

    fn insert(
        &self,
        shard: &mut Self::Queue,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>) {
        EvictLeastRecentlyUsed.insert(shard, |key| construct((self.clock.now().into(), key)))
    }

    fn touch(&self, shard: impl UpgradeReadGuard<Target = Self::Queue>, state: &Self::Value, entry: &P) {
        let now = self.clock.now();
        let since_touched = now
            .checked_duration_since(state.0.load(Ordering::Relaxed))
            .unwrap_or_default();
        if since_touched >= self.hard_window {
            state.0.store(now, Ordering::Relaxed);
            EvictLeastRecentlyUsed.touch(shard, &state.1, entry);
        } else if since_touched >= self.soft_window {
            if let Some(mut write) = UpgradeReadGuard::try_upgrade(shard) {
                state.0.store(now, Ordering::Relaxed);
                write.move_to_tail(state.1);
            }
        }
    }

    fn remove(&self, shard: &mut Self::Queue, value: &Self::Value) {
        EvictLeastRecentlyUsed.remove(shard, &value.1);
    }

    fn replace(
        &self,
        shard: &mut Self::Queue,
        remove: &Self::Value,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>) {
        EvictLeastRecentlyUsed.replace(shard, &remove.1, |key| {
            construct((self.clock.now().into(), key))
        })
    }
}

#[derive(Debug)]
pub struct ApproxLeastRecentlyUsedIntrusiveEviction<C = DefaultClock> {
    clock: C,
    soft_window: Duration,
    hard_window: Duration,
}

impl<C: Default> ApproxLeastRecentlyUsedIntrusiveEviction<C> {
    pub fn new(soft_window: Duration) -> Self {
        Self::with_hard_window(soft_window, soft_window * 2)
    }

    pub fn with_hard_window(soft_window: Duration, hard_window: Duration) -> Self {
        Self::with_clock(soft_window, hard_window, C::default())
    }
}

impl<C> ApproxLeastRecentlyUsedIntrusiveEviction<C> {
    pub fn with_clock(soft_window: Duration, hard_window: Duration, clock: C) -> Self {
        Self {
            clock,
            soft_window,
            hard_window,
        }
    }
}

impl<C, P> Eviction<P> for ApproxLeastRecentlyUsedIntrusiveEviction<C>
where 
    C: Clock,
    P: Deref + Clone,
    P::Target: TouchedTime
{
    type Value = <EvictLeastRecentlyUsed as Eviction<P>>::Value;
    type Queue = <EvictLeastRecentlyUsed as Eviction<P>>::Queue;

    fn new_queue(&mut self, capacity: usize) -> Self::Queue {
        EvictLeastRecentlyUsed.new_queue(capacity)
    }

    fn insert(
        &self,
        shard: &mut Self::Queue,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>) {
        EvictLeastRecentlyUsed.insert(shard, construct)
    }

    fn touch(&self, shard: impl UpgradeReadGuard<Target = Self::Queue>, state: &Self::Value, entry: &P) {
        let now = self.clock.now();
        let since_touched = now.checked_duration_since(entry.last_touched()).unwrap_or_default();

        if since_touched >= self.hard_window {
            entry.touch(now);
            EvictLeastRecentlyUsed.touch(shard, state, entry);
        } else if since_touched >= self.soft_window {
            if let Some(mut write) = UpgradeReadGuard::try_upgrade(shard) {
                entry.touch(now);
                write.move_to_tail(*state);
            }
        }
    }

    fn remove(&self, shard: &mut Self::Queue, value: &Self::Value) {
        EvictLeastRecentlyUsed.remove(shard, value);
    }

    fn replace(
        &self,
        shard: &mut Self::Queue,
        remove: &Self::Value,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>) {
        EvictLeastRecentlyUsed.replace(shard, remove, construct)
    }
}