use std::time::{Duration, Instant};

use super::{
    Pointer,
    layer::{BuildLayer, Layer, LayerDeref, ReadExclusive, ReadShared},
};

pub trait Expire<P: Pointer> {
    type Value: 'static;

    fn insert(&self, target: &P::Target) -> Self::Value;

    fn is_expired(&self, pointer: &P, value: &Self::Value) -> bool;
}

pub struct ExpireLayer<E>(E);

impl<P, D, E> Layer<P, D> for ExpireLayer<E>
where
    P: Pointer,
    D: LayerDeref<P, E::Value>,
    E: Expire<P>,
{
    type Value = E::Value;

    fn create_insert_shared_value(&self, target: &P::Target) -> Self::Value {
        self.0.insert(target)
    }

    fn create_insert_value(&mut self, target: &P::Target) -> Self::Value {
        self.0.insert(target)
    }

    #[inline]
    fn read_shared(&self, pointer: &P) -> ReadShared {
        if self.0.is_expired(pointer, D::deref(pointer)) {
            ReadShared::Remove
        } else {
            ReadShared::Allow
        }
    }

    fn read(&mut self, pointer: &P) -> ReadExclusive {
        if self.0.is_expired(pointer, D::deref(pointer)) {
            ReadExclusive::Remove
        } else {
            ReadExclusive::Allow
        }
    }
}

pub trait Now {
    fn now(&self) -> Instant;
}

#[derive(Debug, Clone, Copy)]
pub struct SystemInstant;

impl Now for SystemInstant {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ExpireAfterWrite<N = SystemInstant> {
    now: N,
    duration: Duration,
}

impl<P: Pointer, N: Now> Expire<P> for ExpireAfterWrite<N> {
    type Value = Instant;

    fn insert(&self, _target: &P::Target) -> Self::Value {
        self.now.now() + self.duration
    }

    fn is_expired(&self, _pointer: &P, value: &Self::Value) -> bool {
        self.now.now() >= *value
    }
}

impl<T, N: Now> BuildLayer<T> for ExpireAfterWrite<N> {
    type Value = Instant;

    type Layer<P, D>
        = ExpireLayer<Self>
    where
        P: Pointer<Target = T>,
        D: LayerDeref<P, Self::Value>;

    fn build<P, D>(self) -> Self::Layer<P, D>
    where
        P: Pointer<Target = T>,
        D: LayerDeref<P, Self::Value>,
    {
        ExpireLayer(self)
    }
}
