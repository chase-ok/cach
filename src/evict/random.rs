use std::marker::PhantomData;

use rand::{thread_rng, Rng, SeedableRng};
use stable_deref_trait::CloneStableDeref;

use crate::layer;

use super::bag::{Bag, Key};

pub struct EvictRandom<R = rand::rngs::SmallRng>(PhantomData<R>);

impl<R> Default for EvictRandom<R> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

#[doc(hidden)]
pub struct Shard<P, R> {
    bag: Bag<P>,
    rng: R,
}

// XX requires clone stable for atomics
impl<P: CloneStableDeref, R: Rng + SeedableRng> layer::Layer<P> for EvictRandom<R> {
    type Value = Key;
    type Shard = Shard<P, R>;

    fn new_shard(&self, capacity: usize) -> Self::Shard {
        assert!(capacity > 0);
        Shard {
            bag: Bag::with_capacity(capacity),
            rng: R::from_rng(thread_rng()).unwrap(),
        }
    }
}

impl<P: CloneStableDeref, G: Rng + SeedableRng> layer::Shard<P> for Shard<P, G> {
    type Value = Key;

    fn write<R: layer::Resolve<P, Self::Value>>(
        &mut self,
        mut write: impl layer::Write<P, Self::Value>,
    ) -> P {
        if self.bag.len() == self.bag.capacity() {
            if let Some(removed) = self.bag.pop(|len| self.rng.gen_range(0..len), R::resolve) {
                write.remove(&removed);
            }
        }
        self.bag
            .insert_with_key(move |key| write.insert(key))
            .clone()
    }

    fn remove<R: layer::Resolve<P, Self::Value>>(&mut self, pointer: &P) {
        self.bag.remove(pointer, R::resolve);
    }

    const READ_LOCK: layer::ReadLock = layer::ReadLock::None;
    const ITER_READ_LOCK: layer::ReadLock = layer::ReadLock::None;
}
