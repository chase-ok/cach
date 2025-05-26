// use super::BuildStore;

use std::{marker::PhantomData, ops::BitAnd};

use super::Pointer;

pub trait BuildLayer<T> {
    type Value: 'static;

    type Layer<P, D>: Layer<P, D, Value = Self::Value>
    where
        P: Pointer<Target = T>,
        D: LayerDeref<P, Self::Value>;

    fn build<P, D>(self) -> Self::Layer<P, D>
    where
        P: Pointer<Target = T>,
        D: LayerDeref<P, Self::Value>;

    fn and_then<N>(self, next: N) -> AndThen<Self, N>
    where
        Self: Sized,
    {
        AndThen {
            layer_0: self,
            layer_1: next,
        }
    }
}

pub trait LayerDeref<P, V> {
    fn deref(pointer: &P) -> &V;
}

pub trait Layer<P: Pointer, D: LayerDeref<P, Self::Value>> {
    type Value: 'static;

    #[inline]
    fn read_lock() -> Lock {
        Lock::Shared
    }

    #[inline]
    fn read_shared(&self, _pointer: &P) -> ReadShared {
        ReadShared::Allow
    }

    #[inline]
    fn read(&mut self, _pointer: &P) -> ReadExclusive {
        ReadExclusive::Allow
    }

    #[inline]
    fn remove_lock() -> Lock {
        Lock::Shared
    }

    #[inline]
    fn remove_shared(&self, _pointer: &P) -> RemoveShared {
        RemoveShared::Allow
    }

    #[inline]
    fn remove(&mut self, _pointer: &P) {}

    #[inline]
    fn insert_lock() -> Lock {
        Lock::Shared
    }

    #[inline]
    fn start_insert_shared(&self, _target: &P::Target) -> InsertShared {
        InsertShared::Allow
    }

    fn create_insert_shared_value(&self, target: &P::Target) -> Self::Value;

    #[inline]
    fn fail_insert_shared(&self, _target: &P::Target, _value: Self::Value) {}

    #[inline]
    fn complete_insert_shared(&self, _pointer: &P) {}

    #[inline]
    fn start_insert(&mut self, _target: &P::Target) -> Insert {
        Insert::Allow
    }

    fn create_insert_value(&mut self, target: &P::Target) -> Self::Value;

    #[inline]
    fn complete_insert(&mut self, _pointer: &P) {}

    #[inline]
    fn purge_lock() -> Lock {
        Lock::Shared
    }

    #[inline]
    fn start_purge_shared(&self) -> PurgeShared<impl Iterator<Item = P>> {
        PurgeShared::Purge(std::iter::empty())
    }

    #[inline]
    fn complete_purge_shared(&self, _pointer: &P) {}

    #[inline]
    fn purge(&mut self) -> impl Iterator<Item = P> {
        std::iter::empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lock {
    Shared,
    Exclusive,
}

impl BitAnd for Lock {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        match (self, rhs) {
            (_, Lock::Exclusive) | (Lock::Exclusive, _) => Lock::Exclusive,
            _ => Lock::Shared,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadShared {
    Allow,
    Remove,
    RequireExclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadExclusive {
    Allow,
    Remove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveShared {
    Allow,
    RequireExclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertShared {
    Allow,
    RequirePurge,
    RequireExclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Insert {
    Allow,
    RequirePurge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PurgeShared<I> {
    Purge(I),
    RequireExclusive,
}

#[derive(Debug, Clone, Copy)]
pub struct AndThen<L0, L1> {
    layer_0: L0,
    layer_1: L1,
}

impl<T, L0, L1> BuildLayer<T> for AndThen<L0, L1>
where
    L0: BuildLayer<T>,
    L1: BuildLayer<T>,
{
    type Value = (L0::Value, L1::Value);

    type Layer<P, D>
        = AndThenLayer<
        L0::Layer<P, LayerDeref0<D, L0::Value, L1::Value>>,
        L1::Layer<P, LayerDeref1<D, L0::Value, L1::Value>>,
        L0::Value,
        L1::Value,
    >
    where
        P: Pointer<Target = T>,
        D: LayerDeref<P, Self::Value>;

    fn build<P, D>(self) -> Self::Layer<P, D>
    where
        P: Pointer<Target = T>,
        D: LayerDeref<P, Self::Value>,
    {
        AndThenLayer {
            layer_0: self.layer_0.build(),
            layer_1: self.layer_1.build(),
            _marker: PhantomData,
        }
    }
}

pub struct AndThenLayer<L0, L1, V0, V1> {
    layer_0: L0,
    layer_1: L1,
    _marker: PhantomData<(V0, V1)>,
}

impl<P, D, L0, L1, V0, V1> Layer<P, D> for AndThenLayer<L0, L1, V0, V1>
where
    P: Pointer,
    D: LayerDeref<P, (V0, V1)>,
    L0: Layer<P, LayerDeref0<D, V0, V1>, Value = V0>,
    L1: Layer<P, LayerDeref1<D, V0, V1>, Value = V1>,
    V0: 'static,
    V1: 'static,
{
    type Value = (V0, V1);

    #[inline]
    fn create_insert_shared_value(&self, target: &P::Target) -> Self::Value {
        (
            self.layer_0.create_insert_shared_value(target),
            self.layer_1.create_insert_shared_value(target),
        )
    }

    #[inline]
    fn create_insert_value(&mut self, target: &<P>::Target) -> Self::Value {
        (
            self.layer_0.create_insert_value(target),
            self.layer_1.create_insert_value(target),
        )
    }

    #[inline]
    fn read_lock() -> Lock {
        L0::read_lock() & L1::read_lock()
    }

    #[inline]
    fn read_shared(&self, pointer: &P) -> ReadShared {
        match self.layer_0.read_shared(pointer) {
            ReadShared::Allow => self.layer_1.read_shared(pointer),
            r => r,
        }
    }

    #[inline]
    fn read(&mut self, pointer: &P) -> ReadExclusive {
        match self.layer_0.read(pointer) {
            ReadExclusive::Allow => self.layer_1.read(pointer),
            r => r,
        }
    }

    #[inline]
    fn remove_lock() -> Lock {
        L0::remove_lock() & L1::remove_lock()
    }

    #[inline]
    fn remove_shared(&self, pointer: &P) -> RemoveShared {
        match self.layer_0.remove_shared(pointer) {
            RemoveShared::Allow => self.layer_1.remove_shared(pointer),
            r => r,
        }
    }

    #[inline]
    fn remove(&mut self, pointer: &P) {
        self.layer_0.remove(pointer);
        self.layer_1.remove(pointer);
    }

    #[inline]
    fn insert_lock() -> Lock {
        L0::insert_lock() & L1::insert_lock()
    }

    #[inline]
    fn start_insert_shared(&self, target: &P::Target) -> InsertShared {
        match self.layer_0.start_insert_shared(target) {
            InsertShared::Allow => self.layer_1.start_insert_shared(target),
            i => i,
        }
    }

    #[inline]
    fn fail_insert_shared(&self, target: &P::Target, value: Self::Value) {
        self.layer_0.fail_insert_shared(target, value.0);
        self.layer_1.fail_insert_shared(target, value.1);
    }

    #[inline]
    fn complete_insert_shared(&self, pointer: &P) {
        self.layer_0.complete_insert_shared(pointer);
        self.layer_1.complete_insert_shared(pointer);
    }

    #[inline]
    fn start_insert(&mut self, target: &P::Target) -> Insert {
        match self.layer_0.start_insert(target) {
            Insert::Allow => self.layer_1.start_insert(target),
            i => i,
        }
    }

    #[inline]
    fn complete_insert(&mut self, pointer: &P) {
        self.layer_0.complete_insert(pointer);
        self.layer_1.complete_insert(pointer);
    }

    #[inline]
    fn purge_lock() -> Lock {
        L0::purge_lock() & L1::purge_lock()
    }

    #[inline]
    fn start_purge_shared(&self) -> PurgeShared<impl Iterator<Item = P>> {
        match self.layer_0.start_purge_shared() {
            PurgeShared::Purge(i0) => match self.layer_1.start_purge_shared() {
                PurgeShared::Purge(i1) => PurgeShared::Purge(i0.chain(i1)),
                PurgeShared::RequireExclusive => PurgeShared::RequireExclusive,
            },
            PurgeShared::RequireExclusive => PurgeShared::RequireExclusive,
        }
    }

    #[inline]
    fn complete_purge_shared(&self, pointer: &P) {
        self.layer_0.complete_purge_shared(pointer);
        self.layer_1.complete_purge_shared(pointer);
    }

    #[inline]
    fn purge(&mut self) -> impl Iterator<Item = P> {
        self.layer_0.purge().chain(self.layer_1.purge())
    }
}

#[doc(hidden)]
pub struct LayerDeref0<D, V0, V1>(PhantomData<(D, V0, V1)>);

impl<P, D, V0: 'static, V1: 'static> LayerDeref<P, V0> for LayerDeref0<D, V0, V1>
where
    D: LayerDeref<P, (V0, V1)>,
{
    fn deref(pointer: &P) -> &V0 {
        &D::deref(pointer).0
    }
}

#[doc(hidden)]
pub struct LayerDeref1<D, V0, V1>(PhantomData<(D, V0, V1)>);

impl<P, D, V0: 'static, V1: 'static> LayerDeref<P, V1> for LayerDeref1<D, V0, V1>
where
    D: LayerDeref<P, (V0, V1)>,
{
    fn deref(pointer: &P) -> &V1 {
        &D::deref(pointer).1
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NoneLayer;

impl<T> BuildLayer<T> for NoneLayer {
    type Value = ();

    type Layer<P, D>
        = Self
    where
        P: Pointer<Target = T>,
        D: LayerDeref<P, Self::Value>;

    fn build<P, D>(self) -> Self::Layer<P, D>
    where
        P: Pointer<Target = T>,
        D: LayerDeref<P, Self::Value>,
    {
        self
    }
}

impl<P, D> Layer<P, D> for NoneLayer
where
    P: Pointer,
    D: LayerDeref<P, ()>,
{
    type Value = ();

    #[inline]
    fn create_insert_shared_value(&self, _target: &P::Target) -> Self::Value {
        ()
    }

    #[inline]
    fn create_insert_value(&mut self, _target: &P::Target) -> Self::Value {
        ()
    }
}
