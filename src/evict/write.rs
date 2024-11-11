use std::ops::Deref;
use std::time::Instant;

use crate::expire::ExpireAt;
use crate::time::{Clock, DefaultClock};

use super::index::Key;
use super::list::List;
use super::{Evict, TouchLockHint, UpgradeReadGuard};

#[derive(Debug)]
pub struct EvictLeastRecentlyInserted;

impl<P: Clone> Evict<P> for EvictLeastRecentlyInserted {
    type Value = Key;
    type Queue = List<P>;

    const TOUCH_LOCK_HINT: TouchLockHint = TouchLockHint::NoLock;

    fn new_queue(&mut self, capacity: usize) -> Self::Queue {
        List::with_capacity(capacity)
    }

    fn insert(
        &self,
        queue: &mut Self::Queue,
        construct: impl FnOnce(Self::Value) -> P,
        _deref: impl Fn(&P) -> &Self::Value,
    ) -> (P, impl Iterator<Item = P>) {
        let (value, removed) = queue.push_tail_with_key_and_pop_if_full(construct);
        (value.clone(), removed.into_iter())
    }

    fn touch(&self, _queue: impl UpgradeReadGuard<Target = Self::Queue>, _pointer: &P, _deref: impl Fn(&P) -> &Self::Value) {}

    fn remove(&self, queue: &mut Self::Queue, pointer: &P, deref: impl Fn(&P) -> &Self::Value) {
        queue.remove(*deref(pointer)).unwrap();
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

    const TOUCH_LOCK_HINT: TouchLockHint = TouchLockHint::NoLock;

    fn new_queue(&mut self, capacity: usize) -> Self::Queue {
        List::with_capacity(capacity)
    }

    fn insert(
        &self,
        queue: &mut Self::Queue,
        construct: impl FnOnce(Self::Value) -> P,
        _deref: impl Fn(&P) -> &Self::Value,
    ) -> (P, impl Iterator<Item = P>) {
        let value = queue.push_tail_with_key(construct);
        (value.clone(), drain_expired(queue, self.0.now()))
    }

    fn touch(
        &self,
        _queue: impl UpgradeReadGuard<Target = Self::Queue>,
        _pointer: &P,
        _deref: impl Fn(&P) -> &Self::Value,
    ) {
    }

    fn remove(&self, queue: &mut Self::Queue, pointer: &P, deref: impl Fn(&P) -> &Self::Value) {
        let removed = queue.remove(*deref(pointer));
        debug_assert!(removed.is_some());
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
