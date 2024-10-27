use std::marker::PhantomData;

use rand::{thread_rng, Rng, SeedableRng};
use stable_deref_trait::CloneStableDeref;

use crate::lock::UpgradeReadGuard;

use super::{bag::{Bag, Key}, Evict, Point, TouchLock};

pub struct EvictRandom<R = rand::rngs::SmallRng>(PhantomData<R>);

impl<R> Default for EvictRandom<R> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

#[doc(hidden)]
pub struct Queue<P, R> {
    bag: Bag<P>,
    rng: R,
}

// XX requires clone stable for atomics
impl<P: CloneStableDeref, R: Rng + SeedableRng> Evict<P> for EvictRandom<R> {
    type Value = Key;
    type Queue = Queue<P, R>;

    const TOUCH_LOCK: TouchLock = TouchLock::None;

    fn new_queue(&mut self, capacity: usize) -> Self::Queue {
        assert!(capacity > 0);
        Queue {
            bag: Bag::with_capacity(capacity),
            rng: R::from_rng(thread_rng()).unwrap(),
        }
    }

    fn insert<Pt: Point<P, Self::Value>>(
        &self,
        queue: &mut Self::Queue,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>) {
        let removed = if queue.bag.len() == queue.bag.capacity() {
            queue.bag.pop::<Pt>(|len| queue.rng.gen_range(0..len))
        } else {
            None
        };

        let value = queue.bag.insert_with_key(construct);
        (value.clone(), removed.into_iter())
    }

    fn touch<Pt>(&self, _queue: impl UpgradeReadGuard<Target = Self::Queue>, _pointer: &P) { }

    fn remove<Pt: Point<P, Self::Value>>(&self, queue: &mut Self::Queue, pointer: &P) {
        queue.bag.remove::<Pt>(pointer);
    }

    fn replace<Pt: Point<P, Self::Value>>(
        &self,
        queue: &mut Self::Queue,
        pointer: &P,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>) {
        queue.bag.remove::<Pt>(pointer);
        let value = queue.bag.insert_with_key(construct);
        (value.clone(), std::iter::empty())
    }
}