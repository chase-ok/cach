use std::ops::Deref;
use std::time::Instant;

use crate::expire::ExpireAt;
use crate::time::{Clock, DefaultClock};

use super::index::{IndexList, Key};
use super::{Eviction, UpgradeReadGuard};

#[derive(Debug)]
pub struct EvictLeastRecentlyInserted;

impl<P: Clone> Eviction<P> for EvictLeastRecentlyInserted {
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
            shard.pop_head()
        } else {
            None
        };

        let (_key, value) = shard.push_tail_with_key(construct);
        (value.clone(), removed.into_iter())
    }

    fn touch(
        &self,
        _queue: impl UpgradeReadGuard<Target = Self::Queue>,
        _value: &Self::Value,
        _entry: &P,
    ) {
    }

    fn remove(&self, queue: &mut Self::Queue, value: &Self::Value) {
        queue.remove(*value).unwrap();
    }

    fn replace(
        &self,
        shard: &mut Self::Queue,
        remove: &Self::Value,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>) {
        shard.remove(*remove).unwrap();
        let (_key, value) = shard.push_tail_with_key(construct);
        (value.clone(), std::iter::empty())
    }
}

#[derive(Debug, Default)]
pub struct EvictExpiredLeastRecentlyInserted<Clk = DefaultClock>(Clk);

impl<P> Eviction<P> for EvictExpiredLeastRecentlyInserted
where
    P: Clone + Deref,
    P::Target: ExpireAt,
{
    type Value = Key;
    type Queue = IndexList<P>;

    fn new_queue(&mut self, capacity: usize) -> Self::Queue {
        IndexList::with_capacity(capacity)
    }

    fn insert(
        &self,
        queue: &mut Self::Queue,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>) {
        let (_key, value) = queue.push_tail_with_key(construct);
        (
            value.clone(),
            drain_expired(queue, self.0.now())
        )
    }

    fn touch(
        &self,
        _queue: impl UpgradeReadGuard<Target = Self::Queue>,
        _value: &Self::Value,
        _entry: &P,
    ) {
    }

    fn remove(&self, queue: &mut Self::Queue, value: &Self::Value) {
        queue.remove(*value).unwrap();
    }

    fn replace(
        &self,
        queue: &mut Self::Queue,
        remove: &Self::Value,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>) {
        queue.remove(*remove).unwrap();
        let (_key, value) = queue.push_tail_with_key(construct);
        (
            value.clone(),
            drain_expired(queue, self.0.now())
        )
    }
}

fn drain_expired<P>(queue: &mut IndexList<P>, now: Instant) -> impl Iterator<Item = P> + '_
where
    P: Clone + Deref,
    P::Target: ExpireAt,
{
    queue
        .drain()
        .take_while(move |p| p.expire_at() <= now)
        .take(8)
}
