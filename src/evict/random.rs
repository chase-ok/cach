use std::{
    marker::PhantomData,
    ops::Deref,
    sync::{atomic::Ordering, Arc},
    time::Instant,
};

use rand::{thread_rng, Rng, SeedableRng};

use crate::{
    layer::{self, ReadLock},
    time::{AtomicInstant, Clock, DefaultClock, WrittenTime},
};

use super::bag::{Bag, Key};

pub struct EvictRandom<G = rand::rngs::SmallRng>(PhantomData<G>);

impl<G> Default for EvictRandom<G> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

#[doc(hidden)]
pub struct RandomShard<P, G> {
    bag: Bag<P>,
    rng: G,
}

impl<P: Deref + Clone, G: Rng + SeedableRng> layer::Layer<P> for EvictRandom<G> {
    type Value = Key;
    type Shard = RandomShard<P, G>;

    fn new_shard(&self, capacity: usize) -> Self::Shard {
        assert!(capacity > 0);
        RandomShard {
            bag: Bag::with_capacity(capacity),
            rng: G::from_rng(thread_rng()).unwrap(),
        }
    }
}

impl<P: Deref + Clone, G: Rng + SeedableRng> layer::Shard<P> for RandomShard<P, G> {
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
            .insert_with_key(move |key| write.write(key))
            .clone()
    }

    fn remove<R: layer::Resolve<P, Self::Value>>(&mut self, pointer: &P) {
        self.bag.remove_by_value(pointer, R::resolve);
    }

    const READ_LOCK: layer::ReadLock = layer::ReadLock::None;
    const ITER_READ_LOCK: layer::ReadLock = layer::ReadLock::None;
}

pub struct EvictLeastOfN<S, G = rand::rngs::SmallRng> {
    strategy: Arc<S>,
    n: u32,
    _random: PhantomData<G>,
}

impl<S: Default, G> Default for EvictLeastOfN<S, G> {
    fn default() -> Self {
        Self {
            strategy: Default::default(),
            n: 2,
            _random: PhantomData,
        }
    }
}

impl<S: Default, G> EvictLeastOfN<S, G> {
    pub fn new(n: u32) -> Self {
        Self::with_strategy(n, S::default())
    }
}

impl<S, G> EvictLeastOfN<S, G> {
    pub fn with_strategy(n: u32, strategy: S) -> Self {
        assert!(n > 1);
        Self {
            strategy: Arc::new(strategy),
            n,
            _random: PhantomData,
        }
    }
}

pub trait LeastOfNStrategy<T: ?Sized> {
    type Value;

    fn new_value(&self, target: &T) -> Self::Value;

    /// XX Stable deref guarantee
    fn read(&self, target: &T, value: &Self::Value);

    fn compare(
        &self,
        left_target: &T,
        left_value: &Self::Value,
        right_target: &T,
        right_value: &Self::Value,
    ) -> std::cmp::Ordering;
}

impl<P, S, G> layer::Layer<P> for EvictLeastOfN<S, G>
where
    P: Deref + Clone,
    S: LeastOfNStrategy<P::Target>,
    G: Rng + SeedableRng,
{
    type Value = Key;
    type Shard = BestOfNShard<P, S, G>;

    fn new_shard(&self, capacity: usize) -> Self::Shard {
        assert!(capacity > 0);
        BestOfNShard {
            bag: Bag::with_capacity(capacity),
            strategy: Arc::clone(&self.strategy),
            n: self.n,
            rng: G::from_rng(thread_rng()).unwrap(),
        }
    }
}

pub struct BestOfNShard<P: Deref, S: LeastOfNStrategy<P::Target>, G> {
    bag: Bag<(P, S::Value)>,
    strategy: Arc<S>,
    n: u32,
    rng: G,
}

impl<P, S, G> layer::Shard<P> for BestOfNShard<P, S, G>
where
    P: Deref + Clone,
    S: LeastOfNStrategy<P::Target>,
    G: Rng + SeedableRng,
{
    type Value = Key;

    fn write<R: layer::Resolve<P, Self::Value>>(
        &mut self,
        mut write: impl layer::Write<P, Self::Value>,
    ) -> P {
        if self.bag.len() == self.bag.capacity() {
            let (_key, (pointer, _value)) = self
                .bag
                .iter_random(|len| self.rng.gen_range(0..len), |(p, _v)| R::resolve(p))
                .take(self.n.try_into().unwrap())
                .min_by(|(_k0, (p0, v0)), (_k1, (p1, v1))| self.strategy.compare(&p0, v0, &p1, v1))
                .expect("bag isn't empty");
            let pointer = pointer.clone(); // XX: needed to stop borrowing &bag

            let (pointer, _value) = self
                .bag
                .remove_by_key(R::resolve(&pointer), |(p, _v)| R::resolve(p));
            write.remove(&pointer);
        }

        let value = self.strategy.new_value(write.target());
        let (pointer, _value) = self
            .bag
            .insert_with_key(move |key| (write.write(key), value));
        pointer.clone()
    }

    fn remove<R: layer::Resolve<P, Self::Value>>(&mut self, pointer: &P) {
        self.bag
            .remove_by_key(R::resolve(&pointer), |(p, _v)| R::resolve(p));
    }

    const READ_LOCK: layer::ReadLock = ReadLock::Ref;

    fn read_ref<R: layer::Resolve<P, Self::Value>>(&self, pointer: &P) -> layer::ReadResult {
        let (_pointer, value) = self.bag.get(R::resolve(pointer), Ordering::Relaxed);
        self.strategy.read(&pointer, value);
        layer::ReadResult::Retain
    }

    const ITER_READ_LOCK: layer::ReadLock = ReadLock::None;
}

#[derive(Debug, Default)]
pub struct LeastRecentlyWritten<C = DefaultClock>(C);

pub type EvictLeastRecentlyWrittenOfN = EvictLeastOfN<LeastRecentlyWritten>;

impl<C: Clock, T: ?Sized> LeastOfNStrategy<T> for LeastRecentlyWritten<C> {
    type Value = Instant;

    fn new_value(&self, _target: &T) -> Self::Value {
        self.0.now()
    }

    fn read(&self, _target: &T, _value: &Self::Value) {}

    fn compare(
        &self,
        _left_target: &T,
        left: &Self::Value,
        _right_target: &T,
        right: &Self::Value,
    ) -> std::cmp::Ordering {
        left.cmp(right)
    }
}

#[derive(Debug, Default)]
pub struct LeastRecentlyWrittenIntrusive;

pub type EvictLeastRecentlyWrittenIntrusiveOfN = EvictLeastOfN<LeastRecentlyWrittenIntrusive>;

impl<T: ?Sized + WrittenTime> LeastOfNStrategy<T> for LeastRecentlyWrittenIntrusive {
    type Value = ();

    fn new_value(&self, _target: &T) -> Self::Value {
        ()
    }

    fn read(&self, _target: &T, _value: &Self::Value) {}

    fn compare(
        &self,
        left_target: &T,
        _left_value: &(),
        right_target: &T,
        _right_value: &(),
    ) -> std::cmp::Ordering {
        left_target.written_time().cmp(&right_target.written_time())
    }
}

#[derive(Debug, Default)]
pub struct LeastRecentlyRead<C = DefaultClock>(C);

pub type EvictLeastRecentlyReadOfN = EvictLeastOfN<LeastRecentlyRead>;

impl<C: Clock, T: ?Sized> LeastOfNStrategy<T> for LeastRecentlyRead<C> {
    type Value = AtomicInstant;

    fn new_value(&self, _target: &T) -> Self::Value {
        self.0.now().into()
    }

    fn read(&self, _target: &T, value: &Self::Value) {
        value.store(self.0.now(), Ordering::Relaxed);
    }

    fn compare(
        &self,
        _left_target: &T,
        left: &Self::Value,
        _right_target: &T,
        right: &Self::Value,
    ) -> std::cmp::Ordering {
        left.load(Ordering::Relaxed)
            .cmp(&right.load(Ordering::Relaxed))
    }
}
