use std::{marker::PhantomData, sync::atomic::Ordering, time::Duration};

use crate::{
    lock::UpgradeReadGuard,
    time::{AtomicInstant, Clock, DefaultClock},
};

use super::{Evict, TouchLock};

pub struct EvictApproximate<E, C = DefaultClock> {
    inner: E,
    clock: C,
    window: Duration,
}

impl<E> EvictApproximate<E> {
    pub fn with_window(eviction: E, window: Duration) -> Self {
        Self {
            inner: eviction,
            window,
            clock: DefaultClock,
        }
    }
}

struct PointInner<Pt>(PhantomData<Pt>);

impl<E, C, P> Evict<P> for EvictApproximate<E, C>
where
    E: Evict<P>,
    C: Clock,
{
    type Value = (AtomicInstant, E::Value);
    type Queue = E::Queue;

    const TOUCH_LOCK: TouchLock = TouchLock::MayWrite;

    fn new_queue(&mut self, capacity: usize) -> Self::Queue {
        self.inner.new_queue(capacity)
    }

    fn insert(
        &self,
        queue: &mut Self::Queue,
        construct: impl FnOnce(Self::Value) -> P,
        deref: impl Fn(&P) -> &Self::Value,
    ) -> (P, impl Iterator<Item = P>) {
        self.inner
            .insert(queue, |inner| construct((self.clock.now().into(), inner)), move |p| &deref(p).1)
    }

    fn touch(&self, queue: impl UpgradeReadGuard<Target = Self::Queue>, pointer: &P, deref: impl Fn(&P) -> &Self::Value) {
        let now = self.clock.now();
        let value = deref(pointer);
        let last = value.0.load(Ordering::Relaxed);
        let since_touched = now.checked_duration_since(last).unwrap_or_default();
        if since_touched >= self.window
            && value
                .0
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            self.inner.touch(queue, pointer, move |p| &deref(p).1)
        }
    }

    fn remove(&self, queue: &mut Self::Queue, pointer: &P, deref: impl Fn(&P) -> &Self::Value) {
        self.inner.remove(queue, pointer, move |p| &deref(p).1);
    }

    fn replace(
        &self,
        queue: &mut Self::Queue,
        pointer: &P,
        construct: impl FnOnce(Self::Value) -> P,
        deref: impl Fn(&P) -> &Self::Value,
    ) -> (P, impl Iterator<Item = P>) {
        self.inner
            .replace(queue, pointer, |k| construct((self.clock.now().into(), k)), move |p| &deref(p).1)
    }
}
