use std::{cell::LazyCell, sync::atomic::Ordering, time::{Duration, Instant}};

use crate::{store::layer::{BuildLayer, Layer}, time::{AtomicInstant, Clock, SystemInstant}};

use super::layer::{LayerPointer, Operate, StartRead};



#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpireAfterWriteLayer<C = SystemInstant> {
    clock: C,
    duration: Duration,
}

impl<T, C: Clock> BuildLayer<T> for ExpireAfterWriteLayer<C> {
    type Value = Instant;

    type Layer<P> = Self
    where
        P: LayerPointer<LayerTarget = Self::Value, Target = T>;

    fn build<P>(self) -> Self::Layer<P>
    where
        P: LayerPointer<LayerTarget = Self::Value, Target = T>
    {
        self
    }
}

impl<P, C> Layer<P> for ExpireAfterWriteLayer<C>
where
    P: LayerPointer<LayerTarget = Instant>,
    C: Clock,
{
    type Value = Instant;

    fn operate(&self) -> impl Operate<P> + '_ {
        struct Op<F> {
            now: LazyCell<Instant, F>,
            duration: Duration,
        }

        impl<P, F> Operate<P> for Op<F>
        where
            P: LayerPointer<LayerTarget = Instant>,
            F: FnOnce() -> Instant
        {
            fn start_insert(&mut self, _target: &P::Target) -> Instant {
                *self.now + self.duration
            }

            fn start_read(&mut self, pointer: &P) -> StartRead {
                if *self.now >= *pointer.layer() {
                    StartRead::Remove
                } else {
                    StartRead::Allow
                }
            }
        }

        Op {
            now: LazyCell::new(|| self.clock.now()),
            duration: self.duration,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpireAfterReadLayer<C = SystemInstant> {
    clock: C,
    duration: Duration,
}

impl<T, C: Clock> BuildLayer<T> for ExpireAfterReadLayer<C> {
    type Value = AtomicInstant;

    type Layer<P> = Self
    where
        P: LayerPointer<LayerTarget = Self::Value, Target = T>;

    fn build<P>(self) -> Self::Layer<P>
    where
        P: LayerPointer<LayerTarget = Self::Value, Target = T>
    {
        self
    }
}

impl<P, C> Layer<P> for ExpireAfterReadLayer<C>
where
    P: LayerPointer<LayerTarget = AtomicInstant>,
    C: Clock,
{
    type Value = AtomicInstant;

    fn operate(&self) -> impl Operate<P> + '_ {
        struct Op<F> {
            now: LazyCell<Instant, F>,
            duration: Duration,
        }

        impl<P, F> Operate<P> for Op<F>
        where
            P: LayerPointer<LayerTarget = AtomicInstant>,
            F: FnOnce() -> Instant
        {
            fn start_insert(&mut self, _target: &P::Target) -> AtomicInstant {
                AtomicInstant::new(*self.now + self.duration)
            }

            fn start_read(&mut self, pointer: &P) -> StartRead {
                // XX: should we do Acquire + Release ordering?
                if *self.now >= pointer.layer().load(Ordering::Relaxed) {
                    StartRead::Remove
                } else {
                    StartRead::Allow
                }
            }

            fn complete_read(&mut self, pointer: &P) {
                pointer.layer().store(*self.now + self.duration, Ordering::Relaxed);
            }
        }

        Op {
            now: LazyCell::new(|| self.clock.now()),
            duration: self.duration,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ExpireAtFixedLayer<C = SystemInstant> {
    clock: C,
}

pub trait ExpireAt {
    fn expire_at(&self, now: Instant) -> Instant;
}

impl<T: ExpireAt, C: Clock> BuildLayer<T> for ExpireAtFixedLayer<C> {
    type Value = Instant;

    type Layer<P> = Self
    where
        P: LayerPointer<LayerTarget = Self::Value, Target = T>;

    fn build<P>(self) -> Self::Layer<P>
    where
        P: LayerPointer<LayerTarget = Self::Value, Target = T>
    {
        self
    }
}

impl<P, C> Layer<P> for ExpireAtFixedLayer<C>
where
    P: LayerPointer<LayerTarget = Instant>,
    P::Target: ExpireAt,
    C: Clock,
{
    type Value = Instant;

    fn operate(&self) -> impl Operate<P> + '_ {
        struct Op<F> {
            now: LazyCell<Instant, F>,
        }

        impl<P, F> Operate<P> for Op<F>
        where
            P: LayerPointer<LayerTarget = Instant>,
            P::Target: ExpireAt,
            F: FnOnce() -> Instant
        {
            fn start_insert(&mut self, target: &P::Target) -> Instant {
                target.expire_at(*self.now)
            }

            fn start_read(&mut self, pointer: &P) -> StartRead {
                if *self.now >= *pointer.layer() {
                    StartRead::Remove
                } else {
                    StartRead::Allow
                }
            }
        }

        Op {
            now: LazyCell::new(|| self.clock.now()),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ExpireAtLayer<C = SystemInstant> {
    clock: C,
}

impl<T: ExpireAt, C: Clock> BuildLayer<T> for ExpireAtLayer<C> {
    type Value = AtomicInstant;

    type Layer<P> = Self
    where
        P: LayerPointer<LayerTarget = Self::Value, Target = T>;

    fn build<P>(self) -> Self::Layer<P>
    where
        P: LayerPointer<LayerTarget = Self::Value, Target = T>
    {
        self
    }
}

impl<P, C> Layer<P> for ExpireAtLayer<C>
where
    P: LayerPointer<LayerTarget = AtomicInstant>,
    P::Target: ExpireAt,
    C: Clock,
{
    type Value = AtomicInstant;

    fn operate(&self) -> impl Operate<P> + '_ {
        struct Op<F> {
            now: LazyCell<Instant, F>,
        }

        impl<P, F> Operate<P> for Op<F>
        where
            P: LayerPointer<LayerTarget = AtomicInstant>,
            P::Target: ExpireAt,
            F: FnOnce() -> Instant
        {
            fn start_insert(&mut self, target: &P::Target) -> AtomicInstant {
                AtomicInstant::new(target.expire_at(*self.now))
            }

            fn start_read(&mut self, pointer: &P) -> StartRead {
                if *self.now >= pointer.layer().load(Ordering::Relaxed) {
                    StartRead::Remove
                } else {
                    StartRead::Allow
                }
            }

            fn complete_read(&mut self, pointer: &P) {
                pointer.layer().store(pointer.expire_at(*self.now), Ordering::Relaxed);
            }
        }

        Op {
            now: LazyCell::new(|| self.clock.now()),
        }
    }
}