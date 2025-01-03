use std::{
    ops::Deref,
    sync::{atomic::Ordering, Arc},
    time::Instant,
};

use crate::{
    layer::{Layer, ReadLock, ReadResult, Resolve, Shard, Write},
    time::AtomicInstant,
    Clock, DefaultClock,
};

pub trait Expire {
    fn is_expired(&self) -> bool;
}

#[derive(Debug)]
pub struct ExpireLayer;

impl<P> Layer<P> for ExpireLayer
where
    P: Deref,
    P::Target: Expire,
{
    type Value = ();
    type Shard = ExpireLayer;

    fn new_shard(&self, _capacity: usize) -> Self::Shard {
        ExpireLayer
    }
}

impl<P> Shard<P> for ExpireLayer
where
    P: Deref,
    P::Target: Expire,
{
    type Value = ();

    fn write<R>(&mut self, write: impl Write<P, Self::Value>) -> P {
        write.write(())
    }

    fn remove<R>(&mut self, _pointer: &P) {}

    const READ_LOCK: ReadLock = ReadLock::Ref;

    fn read_ref<R: Resolve<P, Self::Value>>(&self, pointer: &P) -> ReadResult {
        if pointer.is_expired() {
            ReadResult::Remove
        } else {
            ReadResult::Retain
        }
    }

    const ITER_READ_LOCK: ReadLock = ReadLock::Ref;

    fn iter_read_ref<R: Resolve<P, Self::Value>>(&self, pointer: &P) -> ReadResult {
        self.read_ref::<R>(pointer)
    }
}

pub trait ExpireAt {
    fn expire_at(&self) -> Instant;
}

#[derive(Debug, Default)]
pub struct ExpireAtLayer<C = DefaultClock>(Arc<C>);

impl<C> ExpireAtLayer<C> {
    pub fn with_clock(clock: C) -> Self {
        Self(Arc::new(clock))
    }
}

impl<P, C> Layer<P> for ExpireAtLayer<C>
where
    P: Deref,
    P::Target: ExpireAt,
    C: Clock,
{
    type Value = ();
    type Shard = ExpireAtLayer<C>;

    fn new_shard(&self, _capacity: usize) -> Self::Shard {
        Self(Arc::clone(&self.0))
    }
}

impl<P, C> Shard<P> for ExpireAtLayer<C>
where
    P: Deref,
    P::Target: ExpireAt,
    C: Clock,
{
    type Value = ();

    fn write<R>(&mut self, write: impl Write<P, Self::Value>) -> P {
        write.write(())
    }

    fn remove<R: Resolve<P, Self::Value>>(&mut self, _pointer: &P) {}

    const READ_LOCK: ReadLock = ReadLock::Ref;

    fn read_ref<R: Resolve<P, Self::Value>>(&self, pointer: &P) -> ReadResult {
        if pointer.expire_at() <= self.0.now() {
            ReadResult::Remove
        } else {
            ReadResult::Retain
        }
    }

    const ITER_READ_LOCK: ReadLock = ReadLock::Ref;

    fn iter_read_ref<R: Resolve<P, Self::Value>>(&self, pointer: &P) -> ReadResult {
        self.read_ref::<R>(pointer)
    }
}

#[derive(Debug)]
pub struct ExpireAfterWriteLayer<F, C = DefaultClock>(Arc<ExpireAfterWriteInner<F, C>>);

#[derive(Debug)]
struct ExpireAfterWriteInner<F, C> {
    expire_at_fn: F,
    clock: C,
}

impl<F, C: Default> ExpireAfterWriteLayer<F, C> {
    pub fn new(expire_at_fn: F) -> Self {
        Self::with_clock(expire_at_fn, C::default())
    }
}

impl<F, C> ExpireAfterWriteLayer<F, C> {
    pub fn with_clock(expire_at_fn: F, clock: C) -> Self {
        Self(Arc::new(ExpireAfterWriteInner {
            expire_at_fn,
            clock,
        }))
    }
}

impl<P, F, C> Layer<P> for ExpireAfterWriteLayer<F, C>
where
    P: Deref,
    F: Fn(Instant, &P::Target) -> Instant,
    C: Clock,
{
    type Value = Instant;
    type Shard = ExpireAfterWriteLayer<F, C>;

    fn new_shard(&self, _capacity: usize) -> Self::Shard {
        Self(Arc::clone(&self.0))
    }
}

impl<P, F, C> Shard<P> for ExpireAfterWriteLayer<F, C>
where
    P: Deref,
    F: Fn(Instant, &P::Target) -> Instant,
    C: Clock,
{
    type Value = Instant;

    fn write<R>(&mut self, write: impl Write<P, Self::Value>) -> P {
        let expire = (self.0.expire_at_fn)(self.0.clock.now(), write.target());
        write.write(expire)
    }

    fn remove<R>(&mut self, _pointer: &P) {}

    const READ_LOCK: ReadLock = ReadLock::Ref;

    fn read_ref<R: Resolve<P, Self::Value>>(&self, pointer: &P) -> ReadResult {
        if *R::resolve(pointer) <= self.0.clock.now() {
            ReadResult::Remove
        } else {
            ReadResult::Retain
        }
    }

    const ITER_READ_LOCK: ReadLock = ReadLock::Ref;

    fn iter_read_ref<R: Resolve<P, Self::Value>>(&self, pointer: &P) -> ReadResult {
        self.read_ref::<R>(pointer)
    }
}

#[derive(Debug)]
pub struct ExpireAfterReadLayer<F, C = DefaultClock>(Arc<ExpireAfterReadInner<F, C>>);

#[derive(Debug)]
struct ExpireAfterReadInner<F, C> {
    expire_at_fn: F,
    clock: C,
}

impl<F, C: Default> ExpireAfterReadLayer<F, C> {
    pub fn new(expire_at_fn: F) -> Self {
        Self::with_clock(expire_at_fn, C::default())
    }
}

impl<F, C> ExpireAfterReadLayer<F, C> {
    pub fn with_clock(expire_at_fn: F, clock: C) -> Self {
        Self(Arc::new(ExpireAfterReadInner {
            expire_at_fn,
            clock,
        }))
    }
}

impl<P, F, C> Layer<P> for ExpireAfterReadLayer<F, C>
where
    P: Deref,
    F: Fn(Instant, &P::Target) -> Instant,
    C: Clock,
{
    type Value = AtomicInstant;
    type Shard = ExpireAfterReadLayer<F, C>;

    fn new_shard(&self, _capacity: usize) -> Self::Shard {
        Self(Arc::clone(&self.0))
    }
}

impl<P, F, C> Shard<P> for ExpireAfterReadLayer<F, C>
where
    P: Deref,
    F: Fn(Instant, &P::Target) -> Instant,
    C: Clock,
{
    type Value = AtomicInstant;

    fn write<R>(&mut self, write: impl Write<P, Self::Value>) -> P {
        let expire = (self.0.expire_at_fn)(self.0.clock.now(), write.target());
        write.write(expire.into())
    }

    fn remove<R>(&mut self, _pointer: &P) {}

    const READ_LOCK: ReadLock = ReadLock::Ref;

    fn read_ref<R: Resolve<P, Self::Value>>(&self, pointer: &P) -> ReadResult {
        let now = self.0.clock.now();
        let expire =
            R::resolve(pointer).swap((self.0.expire_at_fn)(now, &pointer), Ordering::Relaxed);
        if expire <= now {
            ReadResult::Remove
        } else {
            ReadResult::Retain
        }
    }

    const ITER_READ_LOCK: ReadLock = ReadLock::Ref;

    fn iter_read_ref<R: Resolve<P, Self::Value>>(&self, pointer: &P) -> ReadResult {
        self.read_ref::<R>(pointer)
    }
}
