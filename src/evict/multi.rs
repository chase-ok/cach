use std::{marker::PhantomData, ops::Deref, sync::atomic::Ordering};

use crate::layer::{self, Resolve};

use super::bag::{Bag, Key};

pub struct MultiLayer<K, L0, L1> {
    l0: L0,
    l1: L1,
    key_fn: K,
}

pub enum Value<V0, V1> {
    V0(V0),
    V1(V1),
}

pub struct Shard<K, S0, S1> {
    key_fn: K,
    s0: S0,
    s1: S1,
}

// impl<K, P, L0, L1> layer::Layer<P> for MultiLayer<K, L0, L1>
// where
//     P: Deref,
//     K: Fn(&P::Target) -> bool + Clone,
//     L0: layer::Layer<P>,
//     L1: layer::Layer<P>,
//     L0::Value: 'static,
//     L1::Value: 'static,
// {
//     type Value = Value<L0::Value, L1::Value>;
//     type Shard = Shard<K, L0::Shard, L1::Shard>;

//     fn new_shard(&self, capacity: usize) -> Self::Shard {
//         Shard {
//             key_fn: self.key_fn.clone(),
//             s0: self.l0.new_shard(capacity),
//             s1: self.l1.new_shard(capacity),
//         }
//     }

//     // XX combine with generation
//     // const TOUCH_LOCK_HINT: TouchLockHint = match (E0::TOUCH_LOCK_HINT, E1::TOUCH_LOCK_HINT) {
//     //     (TouchLockHint::RequireWrite, TouchLockHint::RequireWrite) => TouchLockHint::RequireWrite,
//     //     _ => TouchLockHint::MayWrite,
//     // };

//     // fn new_queue(&mut self, capacity: usize) -> Self::Queue {
//     //     // XX: this interface isn't very flexible for queues of different capacities
//     //     Shard {
//     //         values: Bag::with_capacity(capacity),
//     //         s0: self.e0.new_queue(capacity),
//     //         s1: self.e1.new_queue(capacity),
//     //     }
//     // }

//     // fn insert(
//     //     &self,
//     //     queue: &mut Self::Queue,
//     //     construct: impl FnOnce(Self::Value) -> P,
//     //     deref: impl Fn(&P) -> &Self::Value,
//     // ) -> (P, impl Iterator<Item = P>) {
//     //     // XX: switch to serde style visitors so we can "preview" the value to select which queue to use
//     // }

//     // fn touch(
//     //     &self,
//     //     queue: impl crate::lock::UpgradeReadGuard<Target = Self::Queue>,
//     //     pointer: &P,
//     //     deref: impl Fn(&P) -> &Self::Value,
//     // ) {
//     //     todo!()
//     // }

//     // fn remove(&self, queue: &mut Self::Queue, pointer: &P, deref: impl Fn(&P) -> &Self::Value) {
//     //     match (self.key)(&pointer) {
//     //         0 => self.e0.remove(&mut queue.q0, pointer, |p| match queue.values.get(deref(p), Ordering::Relaxed) {
//     //             MultiLayerValue::V0(v) => v,
//     //             _ => unreachable!()
//     //         }),
//     //         1 => self.e1.remove(&mut queue.q1, pointer, |p| match queue.values.get(deref(p), Ordering::Relaxed) {
//     //             MultiLayerValue::V1(v) => v,
//     //             _ => unreachable!()
//     //         }),
//     //         _ => unreachable!()
//     //     }
//     // }
// }

// impl<K, P, S0, S1> layer::Shard<P> for Shard<K, S0, S1>
// where
//     K: Fn(&P::Target) -> bool + Clone,
//     P: Deref,
//     S0: layer::Shard<P>,
//     S1: layer::Shard<P>,
// {
//     type Value = Value<S0::Value, S1::Value>;

//     fn write<R: layer::Resolve<P, Self::Value>>(
//         &mut self,
//         write: impl layer::Write<P, Self::Value>,
//     ) -> P {
//         match (self.key_fn)(write.target()) {
//             false => self.s0.write::<Resolve0<R, _, _>>(Write0(write)),
//             true => self.s1.write::<Resolve1<R, _, _>>(Write1(write)),
//         }
//     }

//     fn remove<R: layer::Resolve<P, Self::Value>>(&mut self, pointer: &P) {
//         match R::resolve(pointer) {
//             Value::V0(_) => self.s0.remove::<Resolve0<R, _, _>>(pointer),
//             Value::V1(_) => self.s1.remove::<Resolve1<R, _, _>>(pointer),
//         }
//     }

//     const READ_LOCK_BEHAVIOR: layer::ReadLockBehavior =
//         S0::READ_LOCK_BEHAVIOR.and(S1::READ_LOCK_BEHAVIOR);

//     fn read<'a, R: layer::Resolve<P, Self::Value>>(
//         this: impl UpgradeReadGuard<Target = Self>,
//         pointer: &P,
//     ) -> layer::ReadResult {
//         match R::resolve(pointer) {
//             Value::V0(_) => S0::read::<Resolve0<R, _, _>>(
//                 MapUpgradeReadGuard::new(this, |s| &s.s0, |s| &mut s.s0),
//                 pointer,
//             ),
//             Value::V1(_) => S1::read::<Resolve1<R, _, _>>(
//                 MapUpgradeReadGuard::new(this, |s| &s.s1, |s| &mut s.s1),
//                 pointer,
//             ),
//         }
//     }

//     fn iter_read<R: layer::Resolve<P, Self::Value>>(
//         this: impl UpgradeReadGuard<Target = Self>,
//         pointer: &P,
//     ) -> layer::ReadResult {
//         match R::resolve(pointer) {
//             Value::V0(_) => S0::iter_read::<Resolve0<R, _, _>>(
//                 MapUpgradeReadGuard::new(this, |s| &s.s0, |s| &mut s.s0),
//                 pointer,
//             ),
//             Value::V1(_) => S1::iter_read::<Resolve1<R, _, _>>(
//                 MapUpgradeReadGuard::new(this, |s| &s.s1, |s| &mut s.s1),
//                 pointer,
//             ),
//         }
//     }
// }

// struct Resolve0<R, V0, V1>(PhantomData<(R, V0, V1)>);
// impl<P, R, V0, V1> layer::Resolve<P, V0> for Resolve0<R, V0, V1>
// where
//     P: Deref,
//     R: Resolve<P, Value<V0, V1>>,
//     V0: 'static,
//     V1: 'static,
// {
//     fn resolve(pointer: &P) -> &V0 {
//         match R::resolve(pointer) {
//             Value::V0(v) => v,
//             Value::V1(_) => unreachable!(),
//         }
//     }
// }

// struct Resolve1<R, V0, V1>(PhantomData<(R, V0, V1)>);
// impl<P, R, V0, V1> layer::Resolve<P, V1> for Resolve1<R, V0, V1>
// where
//     P: Deref,
//     R: Resolve<P, Value<V0, V1>>,
//     V0: 'static,
//     V1: 'static,
// {
//     fn resolve(pointer: &P) -> &V1 {
//         match R::resolve(pointer) {
//             Value::V0(_) => unreachable!(),
//             Value::V1(v) => v,
//         }
//     }
// }
