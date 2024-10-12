use crate::lock::UpgradeReadGuard;

mod index;
pub mod lru;
pub mod lri;

pub trait Eviction<P> {
    type Value;
    type Queue;

    fn new_queue(&mut self, capacity: usize) -> Self::Queue;

    fn insert(
        &self,
        queue: &mut Self::Queue,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>);

    fn touch(
        &self,
        queue: impl UpgradeReadGuard<Target = Self::Queue>,
        value: &Self::Value,
        pointer: &P,
    );

    fn remove(&self, queue: &mut Self::Queue, value: &Self::Value);

    fn replace(
        &self,
        shard: &mut Self::Queue,
        state: &Self::Value,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>);
}


#[derive(Debug, Default)]
pub struct NoEviction;

impl<P> Eviction<P> for NoEviction {
    type Value = ();
    type Queue = ();

    fn new_queue(&mut self, _capacity: usize) -> Self::Queue {
        ()
    }

    fn insert(
        &self,
        _shard: &mut Self::Queue,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>) {
        (construct(()), std::iter::empty())
    }

    fn touch(
        &self,
        _shard: impl UpgradeReadGuard<Target = Self::Queue>,
        _state: &Self::Value,
        _entry: &P,
    ) { }

    fn remove(&self, _shard: &mut Self::Queue, _state: &Self::Value) { }

    fn replace(
        &self,
        _shard: &mut Self::Queue,
        _state: &Self::Value,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>) {
        (construct(()), std::iter::empty())
    }
}