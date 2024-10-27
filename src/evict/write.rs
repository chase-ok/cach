use std::ops::Deref;
use std::time::Instant;

use crate::expire::ExpireAt;
use crate::time::{Clock, DefaultClock};

use super::index::Key;
use super::list::List;
use super::{Evict, Point, TouchLock, UpgradeReadGuard};

#[derive(Debug)]
pub struct EvictLeastRecentlyInserted;

impl<P: Clone> Evict<P> for EvictLeastRecentlyInserted {
    type Value = Key;
    type Queue = List<P>;

    const TOUCH_LOCK: TouchLock = TouchLock::None;

    fn new_queue(&mut self, capacity: usize) -> Self::Queue {
        List::with_capacity(capacity)
    }

    fn insert<Pt>(
        &self,
        queue: &mut Self::Queue,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>) {
        let (value, removed) = queue.push_tail_with_key_and_pop_if_full(construct);
        (value.clone(), removed.into_iter())
    }

    fn touch<Pt>(&self, _queue: impl UpgradeReadGuard<Target = Self::Queue>, _pointer: &P) {}

    fn remove<Pt: Point<P, Self::Value>>(&self, queue: &mut Self::Queue, pointer: &P) {
        queue.remove(*Pt::point(pointer)).unwrap();
    }

    fn replace<Pt: Point<P, Self::Value>>(
        &self,
        queue: &mut Self::Queue,
        pointer: &P,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>) {
        queue.remove(*Pt::point(pointer)).unwrap();
        let value = queue.push_tail_with_key(construct);
        (value.clone(), std::iter::empty())
    }
}

#[derive(Debug, Default)]
pub struct EvictExpiredLeastRecentlyInserted<Clk = DefaultClock>(Clk);

impl<P> Evict<P> for EvictExpiredLeastRecentlyInserted
where
    P: Clone + Deref,
    P::Target: ExpireAt,
{
    type Value = Key;
    type Queue = List<P>;

    const TOUCH_LOCK: TouchLock = TouchLock::None;

    fn new_queue(&mut self, capacity: usize) -> Self::Queue {
        List::with_capacity(capacity)
    }

    fn insert<Pt>(
        &self,
        queue: &mut Self::Queue,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>) {
        let value = queue.push_tail_with_key(construct);
        (value.clone(), drain_expired(queue, self.0.now()))
    }

    fn touch<Pt>(&self, _queue: impl UpgradeReadGuard<Target = Self::Queue>, _pointer: &P) {}

    fn remove<Pt: Point<P, Self::Value>>(&self, queue: &mut Self::Queue, pointer: &P) {
        let removed = queue.remove(*Pt::point(pointer));
        debug_assert!(removed.is_some());
    }

    fn replace<Pt: Point<P, Self::Value>>(
        &self,
        queue: &mut Self::Queue,
        pointer: &P,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>) {
        queue.remove(*Pt::point(pointer)).unwrap();
        let value = queue.push_tail_with_key(construct);
        (value.clone(), drain_expired(queue, self.0.now()))
    }
}

fn drain_expired<P>(queue: &mut List<P>, now: Instant) -> impl Iterator<Item = P> + '_
where
    P: Clone + Deref,
    P::Target: ExpireAt,
{
    queue
        .drain()
        .take_while(move |p| p.expire_at() <= now)
        .take(8)
}
