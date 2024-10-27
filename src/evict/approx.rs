use std::{marker::PhantomData, sync::atomic::Ordering, time::Duration};

use crate::{
    lock::UpgradeReadGuard,
    time::{AtomicInstant, Clock, DefaultClock},
};

use super::{Evict, Point, TouchLock};

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

impl<Pt, P, T> Point<P, T> for PointInner<Pt>
where 
    Pt: Point<P, (AtomicInstant, T)>
{
    fn point(pointer: &P) -> &T {
        &Pt::point(pointer).1
    }
}

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

    fn insert<Pt: Point<P, Self::Value>>(
        &self,
        queue: &mut Self::Queue,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>) {
        self.inner
            .insert::<PointInner<Pt>>(queue, |inner| construct((self.clock.now().into(), inner)))
    }

    fn touch<Pt: Point<P, Self::Value>>(&self, queue: impl UpgradeReadGuard<Target = Self::Queue>, pointer: &P) {
        let now = self.clock.now();
        let value = Pt::point(pointer);
        let last = value.0.load(Ordering::Relaxed);
        let since_touched = now.checked_duration_since(last).unwrap_or_default();
        if since_touched >= self.window
            && value
                .0
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            self.inner.touch::<PointInner<Pt>>(queue, pointer)
        }
    }

    fn remove<Pt: Point<P, Self::Value>>(&self, queue: &mut Self::Queue, pointer: &P) {
        self.inner.remove::<PointInner<Pt>>(queue, pointer);
    }

    fn replace<Pt: Point<P, Self::Value>>(
        &self,
        queue: &mut Self::Queue,
        pointer: &P,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>) {
        self.inner
            .replace::<PointInner<Pt>>(queue, pointer, |k| construct((self.clock.now().into(), k)))
    }
}
