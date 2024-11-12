use std::ops::Deref;

use crate::layer;
use crate::lock::UpgradeReadGuard;

use super::index::Key;
use super::list::List;

#[derive(Debug)]
pub struct EvictLeastRecentlyTouched;

pub struct Shard<P>(List<P>);

impl<P: Deref + Clone> layer::Layer<P> for EvictLeastRecentlyTouched {
    type Value = Key;
    type Shard = Shard<P>;

    fn new_shard(&self, capacity: usize) -> Self::Shard {
        Shard(List::with_capacity(capacity))
    }
}

impl<P: Clone + Deref> layer::Shard<P> for Shard<P> {
    type Value = Key;

    fn write<R: layer::Resolve<P, Self::Value>>(
        &mut self,
        mut write: impl layer::Write<P, Self::Value>,
    ) -> P {
        if self.0.len() == self.0.capacity() {
            if let Some(removed) = self.0.pop_head() {
                write.remove(&removed);
            }
        }

        self.0.push_tail_with_key(|key| write.insert(key)).clone()
    }

    fn remove<R: layer::Resolve<P, Self::Value>>(&mut self, pointer: &P) {
        let _ = self.0.remove(*R::resolve(pointer));
        // XX: debug assert?
    }

    const READ_LOCK_BEHAVIOR: layer::ReadLockBehavior = layer::ReadLockBehavior::RequireWriteLock;

    fn read<'a, R: layer::Resolve<P, Self::Value>>(
        this: impl crate::lock::UpgradeReadGuard<Target = Self>,
        pointer: &P,
    ) -> layer::ReadResult {
        // XX: doc that require write lock => atomic!
        // XX: need to that value isn't removed in between
        this.atomic_upgrade().0.move_to_tail(*R::resolve(pointer));
        layer::ReadResult::Retain
    }

    fn iter_read<R: layer::Resolve<P, Self::Value>>(
        _this: impl crate::lock::UpgradeReadGuard<Target = Self>,
        _pointer: &P,
    ) -> layer::ReadResult {
        // Don't shuffle read order based on iter()
        layer::ReadResult::Retain
    }
}
