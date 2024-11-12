use std::ops::Deref;

use crate::layer;

use super::index::Key;
use super::list::List;

#[derive(Debug)]
pub struct EvictLeastRecentlyInserted;

pub struct Shard<P>(List<P>);

impl<P: Clone + Deref> layer::Layer<P> for EvictLeastRecentlyInserted {
    type Value = Key;
    type Shard = Shard<P>;

    fn new_shard(&self, capacity: usize) -> Self::Shard {
        Shard(List::with_capacity(capacity))
    }
}

impl<P: Clone + Deref> layer::Shard<P> for Shard<P> {
    type Value = Key;

    fn write<R: layer::Resolve<P, Self::Value>>(&mut self, mut write: impl layer::Write<P, Self::Value>) -> P {
        if self.0.len() == self.0.capacity() {
            if let Some(removed) = self.0.pop_head() {
                write.remove(&removed);
            }
        }

        self.0.push_tail_with_key(|key| write.insert(key)).clone()
    }

    const READ_LOCK_BEHAVIOR: layer::ReadLockBehavior = layer::ReadLockBehavior::ReadLockOnly;

    fn remove<R: layer::Resolve<P, Self::Value>>(&mut self, pointer: &P) {
        let _ = self.0.remove(*R::resolve(pointer));
        // XX debug assert?
    }
}