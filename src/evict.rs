use crate::lock::UpgradeReadGuard;

mod approx;
pub mod generation;
pub mod touch;
pub mod write;

#[cfg(feature = "rand")]
pub mod random;

#[cfg(feature = "rand")]
mod bag;

mod list;
mod index;

pub use approx::EvictApproximate;

pub trait Evict<P> {
    type Value;
    type Queue;

    const TOUCH_LOCK: TouchLock;

    fn new_queue(&mut self, capacity: usize) -> Self::Queue;

    fn insert<Pt: Point<P, Self::Value>>(
        &self,
        queue: &mut Self::Queue,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>);

    fn touch<Pt: Point<P, Self::Value>>(&self, queue: impl UpgradeReadGuard<Target = Self::Queue>, pointer: &P);

    fn remove<Pt: Point<P, Self::Value>>(&self, queue: &mut Self::Queue, pointer: &P);

    fn replace<Pt: Point<P, Self::Value>>(
        &self,
        queue: &mut Self::Queue,
        pointer: &P,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>);
}

// XX: requires stable deref to T

#[doc(hidden)]
pub trait Point<P, T: ?Sized> {
    fn point(pointer: &P) -> &T;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchLock {
    None,
    MayWrite,
    RequireWrite,
}

#[derive(Debug, Default)]
pub struct EvictNone;

impl<P> Evict<P> for EvictNone {
    type Value = ();
    type Queue = ();

    const TOUCH_LOCK: TouchLock = TouchLock::None;

    fn new_queue(&mut self, _capacity: usize) -> Self::Queue {
        ()
    }

    fn insert<Pt>(
        &self,
        _queue: &mut Self::Queue,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>) {
        (construct(()), std::iter::empty())
    }

    fn touch<Pt>(&self, _queue: impl UpgradeReadGuard<Target = Self::Queue>, _pointer: &P) {}

    fn remove<Pt>(&self, _queue: &mut Self::Queue, _pointer: &P) {}

    fn replace<Pt>(
        &self,
        _queue: &mut Self::Queue,
        _pointer: &P,
        construct: impl FnOnce(Self::Value) -> P,
    ) -> (P, impl Iterator<Item = P>) {
        (construct(()), std::iter::empty())
    }
}

// pub struct EvictOr<E1, E2>(E1, E2);

// impl<P, E1, E2> Eviction<P> for EvictOr<E1, E2>
// where
//     E1: Eviction<P>,
//     E2: Eviction<P>,
// {
//     type Value = (E1::Value, E2::Value);
//     type Queue = (E1::Value, E2::Value);

//     const TOUCH_LOCK: TouchLock = TouchLock::RequireWrite; // XX

//     fn new_queue(&mut self, capacity: usize) -> Self::Queue {
//         (self.0.new_queue(capacity), self.1.new_queue(capacity))
//     }

//     fn insert(
//         &self,
//         queue: &mut Self::Queue,
//         construct: impl FnOnce(Self::Value) -> P,
//     ) -> (P, impl Iterator<Item = P>) {

//     }

//     fn touch(
//         &self,
//         queue: impl UpgradeReadGuard<Target = Self::Queue>,
//         pointer: &P,
//     ) {
//         todo!()
//     }

//     fn remove(&self, queue: &mut Self::Queue, pointer: &P) {
//         todo!()
//     }

//     fn replace(
//         &self,
//         queue: &mut Self::Queue,
//         pointer: &P,
//         construct: impl FnOnce(Self::Value) -> P,
//     ) -> (P, impl Iterator<Item = P>) {
//         todo!()
//     }
// }
