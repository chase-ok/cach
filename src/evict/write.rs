use std::ops::Deref;

use crate::layer;

use super::index::Key;
use super::list::List;

#[derive(Debug)]
pub struct EvictLeastRecentlyWritten;

pub struct Shard<P>(List<P>);

impl<P: Clone + Deref> layer::Layer<P> for EvictLeastRecentlyWritten {
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

        self.0.push_tail_with_key(|key| write.write(key)).clone()
    }

    fn remove<R: layer::Resolve<P, Self::Value>>(&mut self, pointer: &P) {
        let _ = self.0.remove(*R::resolve(pointer));
        // XX debug assert?
    }

    const READ_LOCK: layer::ReadLock = layer::ReadLock::None;
    const ITER_READ_LOCK: layer::ReadLock = layer::ReadLock::None;
}