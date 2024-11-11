use std::{
    fmt::Debug,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
    time::{Duration, Instant},
};

use smallvec::SmallVec;
use stable_deref_trait::CloneStableDeref;

use crate::{
    lock::{MapUpgradeReadGuard, UpgradeReadGuard},
    time::{Clock, DefaultClock},
};

use super::{Evict, TouchLockHint};

pub trait Promote {
    type Value: 'static;

    fn new_value(&self) -> Self::Value;
    fn try_touch_promote(&self, value: &Self::Value) -> bool;
}

#[derive(Debug)]
pub struct PromoteAfterTouchCount {
    required_touches: u32,
}

impl Default for PromoteAfterTouchCount {
    fn default() -> Self {
        Self {
            required_touches: 3,
        }
    }
}

impl Promote for PromoteAfterTouchCount {
    type Value = AtomicU32;

    fn new_value(&self) -> Self::Value {
        self.required_touches.into()
    }

    fn try_touch_promote(&self, value: &Self::Value) -> bool {
        let update = value.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |t| t.checked_sub(1));
        matches!(update, Ok(0) | Err(_))
    }
}

#[derive(Debug)]
pub struct PromoteTouchedAfterDuration<C = DefaultClock> {
    duration: Duration,
    clock: C,
}

impl<C: Default> Default for PromoteTouchedAfterDuration<C> {
    fn default() -> Self {
        Self {
            duration: Duration::from_secs(1),
            clock: C::default(),
        }
    }
}

impl<C: Clock> Promote for PromoteTouchedAfterDuration<C> {
    type Value = Instant;

    fn new_value(&self) -> Self::Value {
        self.clock
            .now()
            .checked_add(self.duration)
            .expect("duration too long")
    }

    fn try_touch_promote(&self, value: &Self::Value) -> bool {
        *value <= self.clock.now()
    }
}

// XX add Promote::and

// XX: rather than atomic transfer, just store the state as an enum inside a Bag on the queue

pub trait AtomicTransfer {
    fn atomic_transfer(self, other: &Self, order: Ordering);
}

#[derive(Debug, Clone, Copy)]
pub struct EvictGenerational<Promo, G0, G1> {
    promo: Promo,
    g0: G0,
    g1: G1,
    g0_fraction: f64,
}

pub struct Value<P, T> {
    g0: AtomicBool,
    promo: P,
    inner: T,
}

pub struct Queue<P, Q0, Q1> {
    touched_removed: Vec<P>,
    q0: Q0,
    q1: Q1,
}

// XX switch to funcs instead of Point?

impl<P, Promo, G0, G1> Evict<P> for EvictGenerational<Promo, G0, G1>
where
    P: CloneStableDeref,
    Promo: Promote,
    G0: Evict<P>,
    G1: Evict<P, Value = G0::Value>,
    G0::Value: AtomicTransfer,
{
    type Value = Value<Promo::Value, G0::Value>;
    type Queue = Queue<P, G0::Queue, G1::Queue>;

    const TOUCH_LOCK_HINT: TouchLockHint = match (G0::TOUCH_LOCK_HINT, G1::TOUCH_LOCK_HINT) {
        (TouchLockHint::RequireWrite, TouchLockHint::RequireWrite) => TouchLockHint::RequireWrite,
        _ => TouchLockHint::MayWrite,
    };

    fn new_queue(&mut self, capacity: usize) -> Self::Queue {
        let g0_cap = ((self.g0_fraction * (capacity as f64)).round() as usize).max(1);
        Queue {
            touched_removed: Vec::with_capacity(capacity),
            q0: self.g0.new_queue(g0_cap),
            q1: self.g1.new_queue(capacity),
        }
    }

    fn insert(
        &self,
        queue: &mut Self::Queue,
        construct: impl FnOnce(Self::Value) -> P,
        deref: impl Fn(&P) -> &Self::Value,
    ) -> (P, impl Iterator<Item = P>) {
        let (inserted, removed) = self.g0.insert(
            &mut queue.q0,
            |v0| {
                construct(Value {
                    g0: true.into(),
                    promo: self.promo.new_value(),
                    inner: v0,
                })
            },
            move |p| &deref(p).inner,
        );
        // XX: why 4
        (
            inserted,
            removed.chain(std::iter::from_fn(|| queue.touched_removed.pop()).take(4)),
        )
    }

    fn touch(
        &self,
        queue: impl UpgradeReadGuard<Target = Self::Queue>,
        pointer: &P,
        deref: impl Fn(&P) -> &Self::Value,
    ) {
        let value = deref(pointer);
        // XX: can use relaxed since we don't modify g0 except under &mut
        match value.g0.load(Ordering::Relaxed) {
            true => {
                if self.promo.try_touch_promote(&value.promo) {
                    let mut queue = UpgradeReadGuard::upgrade(queue);
                    match value.g0.load(Ordering::Relaxed) {
                        true => {
                            value.g0.store(false, Ordering::Relaxed);
                            self.g0
                                .remove(&mut queue.q0, pointer, |p| &(&deref)(p).inner);
                            let (_pointer, removed) = self.g1.insert(
                                &mut queue.q1,
                                |v1| {
                                    v1.atomic_transfer(&value.inner, Ordering::Relaxed);
                                    pointer.clone()
                                },
                                |p| &(&deref)(p).inner,
                            );
                            let removed = removed.collect::<SmallVec<[_; 8]>>();
                            queue.touched_removed.extend(removed);
                        }
                        false => {
                            // someone else beat us to it, but we still need to touch g1
                            self.g1
                                .touch(&mut queue.q1, pointer, |p| &(&deref)(p).inner);
                        }
                    }
                } else {
                    let queue = MapUpgradeReadGuard::new(queue, |q| &q.q0, |q| &mut q.q0);
                    self.g0.touch(queue, pointer, move |p| &deref(p).inner);
                }
            }
            false => {
                let queue = MapUpgradeReadGuard::new(queue, |q| &q.q1, |q| &mut q.q1);
                self.g1.touch(queue, pointer, move |p| &deref(p).inner);
            }
        }
    }

    fn remove(&self, queue: &mut Self::Queue, pointer: &P, deref: impl Fn(&P) -> &Self::Value) {
        let value = deref(pointer);
        // XX relaxed
        match value.g0.load(Ordering::Relaxed) {
            true => self
                .g0
                .remove(&mut queue.q0, pointer, move |p| &deref(p).inner),
            false => self
                .g1
                .remove(&mut queue.q1, pointer, move |p| &deref(p).inner),
        }
    }
}
