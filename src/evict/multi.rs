use std::{marker::PhantomData, ops::Deref, sync::atomic::Ordering};

use super::{bag::{Bag, Key}, Evict, TouchLockHint};


pub struct MultiEvict<K, E0, E1> {
    e0: E0,
    e1: E1,
    key: K,
}

pub enum MultiEvictValue<V0, V1> {
    V0(V0),
    V1(V1),
}

struct Queue<P, E0: Evict<P>, E1: Evict<P>> {
    values: Bag<MultiEvictValue<E0::Value, E1::Value>>,
    q0: E0::Queue,
    q1: E1::Queue,
}

impl<K, P, E0, E1> Evict<P> for MultiEvict<K, E0, E1> 
where 
    P: Deref,
    K: Fn(&P::Target) -> usize,
    E0: Evict<P>,
    E1: Evict<P>,
    E0::Value: 'static,
    E1::Value: 'static,
{
    type Value = Key;
    type Queue = Queue<P, E0, E1>;

    // XX combine with generation
    const TOUCH_LOCK_HINT: TouchLockHint = match (E0::TOUCH_LOCK_HINT, E1::TOUCH_LOCK_HINT) {
        (TouchLockHint::RequireWrite, TouchLockHint::RequireWrite) => TouchLockHint::RequireWrite,
        _ => TouchLockHint::MayWrite,
    };

    fn new_queue(&mut self, capacity: usize) -> Self::Queue {
        // XX: this interface isn't very flexible for queues of different capacities
        Queue {
            values: Bag::with_capacity(capacity),
            q0: self.e0.new_queue(capacity),
            q1: self.e1.new_queue(capacity),
        }
    }

    fn insert(
        &self,
        queue: &mut Self::Queue,
        construct: impl FnOnce(Self::Value) -> P,
        deref: impl Fn(&P) -> &Self::Value,
    ) -> (P, impl Iterator<Item = P>) {
        // XX: switch to serde style visitors so we can "preview" the value to select which queue to use
    }

    fn touch(
        &self,
        queue: impl crate::lock::UpgradeReadGuard<Target = Self::Queue>,
        pointer: &P,
        deref: impl Fn(&P) -> &Self::Value,
    ) {
        todo!()
    }

    fn remove(&self, queue: &mut Self::Queue, pointer: &P, deref: impl Fn(&P) -> &Self::Value) {
        match (self.key)(&pointer) {
            0 => self.e0.remove(&mut queue.q0, pointer, |p| match queue.values.get(deref(p), Ordering::Relaxed) {
                MultiEvictValue::V0(v) => v,
                _ => unreachable!()
            }),
            1 => self.e1.remove(&mut queue.q1, pointer, |p| match queue.values.get(deref(p), Ordering::Relaxed) {
                MultiEvictValue::V1(v) => v,
                _ => unreachable!()
            }),
            _ => unreachable!()
        }
    }
}