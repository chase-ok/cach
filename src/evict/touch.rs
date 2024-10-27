use super::index::Key;
use super::list::List;
use super::{Evict, TouchLock, UpgradeReadGuard};

#[derive(Debug)]
pub struct EvictLeastRecentlyTouched;

impl<P: Clone> Evict<P> for EvictLeastRecentlyTouched {
    type Value = Key;
    type Queue = List<P>;

    const TOUCH_LOCK: TouchLock = TouchLock::RequireWrite;

    fn new_queue(&mut self, capacity: usize) -> Self::Queue {
        List::with_capacity(capacity)
    }

    fn insert(
        &self,
        queue: &mut Self::Queue,
        construct: impl FnOnce(Self::Value) -> P,
        _deref: impl Fn(&P) -> &Self::Value,
    ) -> (P, impl Iterator<Item = P>) {
        let (value, removed) = queue.push_tail_with_key_and_pop_if_full(construct);
        (value.clone(), removed.into_iter())
    }

    fn touch(
        &self,
        queue: impl UpgradeReadGuard<Target = Self::Queue>,
        pointer: &P,
        deref: impl Fn(&P) -> &Self::Value,
    ) {
        UpgradeReadGuard::upgrade(queue).move_to_tail(*deref(pointer));
    }

    fn remove(&self, queue: &mut Self::Queue, pointer: &P, deref: impl Fn(&P) -> &Self::Value) {
        let removed = queue.remove(*deref(pointer));
        debug_assert!(removed.is_some());
    }

    fn replace(
        &self,
        queue: &mut Self::Queue,
        pointer: &P,
        construct: impl FnOnce(Self::Value) -> P,
        deref: impl Fn(&P) -> &Self::Value,
    ) -> (P, impl Iterator<Item = P>) {
        let removed = queue.remove(*deref(pointer));
        debug_assert!(removed.is_some());
        let value = queue.push_tail_with_key(construct);
        (value.clone(), std::iter::empty())
    }
}
