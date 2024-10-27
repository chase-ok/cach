use std::{marker::PhantomData, sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering}};

use scc::queue;
use smallvec::SmallVec;
use stable_deref_trait::CloneStableDeref;

use crate::lock::UpgradeReadGuard;

use super::{
    index::AtomicKey, list::List, Evict, Point, TouchLock
};

pub trait Promote {
    type Value: 'static;
    const TOUCH_LOCK: TouchLock;

    fn new_value(&self) -> Self::Value;
    fn try_touch_promote(&self, value: &Self::Value) -> bool;
}

pub trait AtomicUsizeValue: Sized + 'static {
    fn into_usize(self) -> usize;
    fn from_atomic_usize_ref(value: &AtomicUsize) -> &Self;
}

#[derive(Debug, Clone, Copy)]
pub struct EvictGenerationalLeastRecentlyTouched<Promo, G0, G1> {
    promo: Promo,
    g0: G0,
    g1: G1,
    g0_fraction: f64,
}

#[doc(hidden)]
pub struct SegmentValue {
    key: AtomicKey,
    gen_and_touches: AtomicGenAndTouches,
}

struct AtomicGenAndTouches(AtomicU64);
type Gen = u8;
type Touches = u32;

impl AtomicGenAndTouches {
    fn new(current: (Gen, Touches)) -> Self {
        Self(gen_and_touches_to_repr(current).into())
    }

    fn load(&self, order: Ordering) -> (Gen, Touches) {
        repr_to_gen_and_touches(self.0.load(order))
    }

    fn compare_exchange_weak(
        &self,
        current: (Gen, Touches),
        new: (Gen, Touches),
        success: Ordering,
        failure: Ordering,
    ) -> Result<(Gen, Touches), (Gen, Touches)> {
        self.0
            .compare_exchange_weak(
                gen_and_touches_to_repr(current),
                gen_and_touches_to_repr(new),
                success,
                failure,
            )
            .map(repr_to_gen_and_touches)
            .map_err(repr_to_gen_and_touches)
    }

    fn store(&self, new: (Gen, Touches), order: Ordering) {
        self.0.store(gen_and_touches_to_repr(new), order);
    }
}

fn gen_and_touches_to_repr((gen, touches): (Gen, Touches)) -> u64 {
    ((gen as u64) << Touches::BITS) & (touches as u64)
}

fn repr_to_gen_and_touches(repr: u64) -> (Gen, Touches) {
    (
        ((repr >> Touches::BITS) & (Gen::MAX as u64)) as u8,
        (repr & (Touches::MAX as u64)) as u32,
    )
}

struct Point0<Pt, T, T0, T1>(PhantomData<(Pt, T, T0, T1)>);

impl<P, Pt, T, T0, T1> Point<P, T0> for Point0<Pt, T, T0, T1>
where 
    Pt: Point<P, Value<T>>,
    T: 'static,
    T0: AtomicUsizeValue, 
{
    fn point(pointer: &P) -> &T0 {
        let value = Pt::point(pointer);
        debug_assert!(value.g0.load(Ordering::Relaxed));
        T0::from_atomic_usize_ref(&value.inner)
    }
}

struct Point1<Pt, T, T0, T1>(PhantomData<(Pt, T, T0, T1)>);

impl<P, Pt, T, T0, T1> Point<P, T1> for Point1<Pt, T, T0, T1>
where 
    Pt: Point<P, Value<T>>,
    T: 'static,
    T1: AtomicUsizeValue, 
{
    fn point(pointer: &P) -> &T1 {
        let value = Pt::point(pointer);
        debug_assert!(value.g0.load(Ordering::Relaxed));
        T1::from_atomic_usize_ref(&value.inner)
    }
}

pub struct Value<T> {
    g0: AtomicBool,
    promo: T,
    inner: AtomicUsize,
}

pub struct Queue<P, Q0, Q1> {
    touched_removed: Vec<P>,
    q0: Q0,
    q1: Q1,
}

// XX switch to funcs instead of Point?

impl<P, Promo, G0, G1> Evict<P> for EvictGenerationalLeastRecentlyTouched<Promo, G0, G1> 
where 
    P: CloneStableDeref,
    Promo: Promote,
    G0: Evict<P>,
    G1: Evict<P>,
    G0::Value: AtomicUsizeValue,
    G1::Value: AtomicUsizeValue,
{
    type Value = Value<Promo::Value>;
    type Queue = Queue<P, G0::Queue, G1::Queue>;

    const TOUCH_LOCK: TouchLock = match Promo::TOUCH_LOCK {
        TouchLock::RequireWrite => TouchLock::RequireWrite,
        _ => TouchLock::MayWrite,
    };

    fn new_queue(&mut self, capacity: usize) -> Self::Queue {
        let g0_cap = ((self.g0_fraction*(capacity as f64)).round() as usize).max(1);
        Queue {
            touched_removed: Vec::with_capacity(capacity),
            q0: self.g0.new_queue(g0_cap),
            q1: self.g1.new_queue(capacity),
        }
    }

    fn insert<Pt: Point<P, Self::Value>>(
        &self,
        queue: &mut Self::Queue,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>) {
        let (inserted, removed) = self.g0.insert::<Point0<Pt, Promo::Value, G0::Value, G1::Value>>(&mut queue.q0, |v0| construct(Value {
            g0: true.into(),
            promo: self.promo.new_value(),
            inner: v0.into_usize().into(),
        }));
        // XX: why 4
        (inserted, removed.chain(std::iter::from_fn(|| queue.touched_removed.pop()).take(4)))
    }

    fn touch<Pt: Point<P, Self::Value>>(&self, queue: impl UpgradeReadGuard<Target = Self::Queue>, pointer: &P) {
        let value = Pt::point(pointer);
        // XX: can use relaxed since we don't modify g0 except under &mut
        match value.g0.load(Ordering::Relaxed) {
            true => {
                if self.promo.try_touch_promote(&value.promo) {
                    let mut queue = UpgradeReadGuard::upgrade(queue);
                    match value.g0.load(Ordering::Relaxed) {
                        true => {
                            value.g0.store(false, Ordering::Relaxed);
                            self.g0.remove::<Point0<Pt, Promo::Value, G0::Value, G1::Value>>(&mut queue.q0, pointer);
                            let (_pointer, removed) = self.g1.insert::<Point1<Pt, Promo::Value, G0::Value, G1::Value>>(&mut queue.q1, |v1| {
                                value.inner.store(v1.into_usize(), Ordering::Relaxed);
                                pointer.clone()
                            });
                            let removed = removed.collect::<SmallVec<[_; 8]>>();
                            queue.touched_removed.extend(removed);
                        }
                        false => {
                            // someone else beat us to it, but we still need to touch g1
                            self.g1.touch::<Point1<Pt, Promo::Value, G0::Value, G1::Value>>(&mut queue.q1, pointer);
                        }
                    }
                }
            }
            false => {
                // XX map
                // self.g1.touch::<Point1<Pt, Promo::Value, G0::Value, G1::Value>>(&mut queue.1, pointer);
            }
        }
    }

    fn remove(&self, queue: &mut Self::Queue, value: &Self::Value) {
        
        // XX safety
        // let segment = value.generation.load(Ordering::Relaxed);
        // let key = value.key.load(Ordering::Relaxed);

        // if let Some(segment) = queue.get_mut(segment as usize) {
        //     let removed = segment.remove(key);
        //     debug_assert!(removed.is_some());
        // } else {
        //     debug_assert!(false, "invalid segment {segment}");
        // }
    }

    fn replace(
        &self,
        shard: &mut Self::Queue,
        value: &Self::Value,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>) {
        let p = todo!();
        (p, std::iter::empty())
    }
}

        // let (mut gen, mut touches) = value.gen_and_touches.load(Ordering::Relaxed);
        // loop {
        //     if self.generations[gen as usize].touches.is_none() {
        //         return;
        //     }

        //     if let Some(new_touches) = touches.checked_sub(1) {
        //         let update = value.gen_and_touches.compare_exchange_weak(
        //             (gen, touches),
        //             (gen, new_touches),
        //             Ordering::Relaxed,
        //             Ordering::Relaxed,
        //         );
        //         match update {
        //             Ok(_) => return,
        //             Err((new_gen, new_touches)) => {
        //                 gen = new_gen;
        //                 touches = new_touches;
        //             }
        //         }
        //     } else {
        //         break;
        //     }
        // }

        // let mut queue = UpgradeReadGuard::upgrade(queue);

        // // refresh now that we've acquired the lock
        // let (gen, touches) = value.gen_and_touches.load(Ordering::Relaxed);
        // if self.generations[gen as usize].touches.is_none() {
        //     return;
        // } else if let Some(new_touches) = touches.checked_sub(1) {
        //     value
        //         .gen_and_touches
        //         .store((gen, new_touches), Ordering::Relaxed);
        // } else {
        //     let key = value.key.load(Ordering::Relaxed);
        //     if queue[gen as usize].remove(key).is_some() {
        //         let new_gen = gen + 1;
        //         // queue[new_gen as usize].push_tail_with_key_and_pop_if_full(value)
        //     } else {
        //         debug_assert!(false, "couldn't find key");
        //     }
        // }