use std::{cell::RefCell, marker::PhantomData};

use parking_lot::Mutex;

use crate::lock::{MutexGuardDetached, RefMutDetached};

use super::{BuildLayer, BuildLayerMut, Layer, LayerMut, LayerPointer};

pub struct Local<B>(B);

impl<B> Local<B> {
    pub fn new(build: B) -> Self {
        Self(build)
    }
}

impl<T, B> BuildLayer<T> for Local<B>
where
    B: BuildLayerMut<T>,
{
    type Value = B::Value;

    type Layer<P>
        = LocalLayer<B::LayerMut<P>>
    where
        P: super::LayerPointer<LayerTarget = Self::Value>;

    fn build<P>(self) -> Self::Layer<P>
    where
        Self: Sized,
        P: super::LayerPointer<LayerTarget = Self::Value>,
    {
        LocalLayer(RefCell::new(self.0.build_mut()))
    }
}

pub struct LocalLayer<L>(RefCell<L>);

impl<P, L> Layer<P> for LocalLayer<L>
where
    P: LayerPointer<LayerTarget = L::Value>,
    L: LayerMut<P>,
{
    type Value = L::Value;

    fn operate(&self) -> impl super::Operate<P> + '_ {

        // XX safety
        let (_guard, layer) = unsafe { RefMutDetached::detach_from(self.0.borrow_mut()) };
        OperateWithGuard {
            op: layer.operate_mut(),
            _guard,
            _marker: PhantomData
        }
    }
}

pub struct Sync<B>(B);

impl<B> Sync<B> {
    pub fn new(build: B) -> Self {
        Self(build)
    }
}

impl<T, B> BuildLayer<T> for Sync<B>
where
    B: BuildLayerMut<T>,
{
    type Value = B::Value;

    type Layer<P>
        = SyncLayer<B::LayerMut<P>>
    where
        P: super::LayerPointer<LayerTarget = Self::Value>;

    fn build<P>(self) -> Self::Layer<P>
    where
        Self: Sized,
        P: super::LayerPointer<LayerTarget = Self::Value>,
    {
        SyncLayer(Mutex::new(self.0.build_mut()))
    }
}

pub struct SyncLayer<L>(Mutex<L>);

impl<P, L> Layer<P> for SyncLayer<L>
where
    P: LayerPointer<LayerTarget = L::Value>,
    L: LayerMut<P>,
{
    type Value = L::Value;

    fn operate(&self) -> impl super::Operate<P> + '_ {
        // XX safety, de-dupe with local
        let (_guard, layer) = unsafe { MutexGuardDetached::detach_from(self.0.lock()) };
        OperateWithGuard {
            op: layer.operate_mut(),
            _guard,
            _marker: PhantomData,
        }
    }
}

struct OperateWithGuard<'a, O, G> {
    op: O,
    _guard: G,
    _marker: PhantomData<&'a ()>
}

impl<O, G, P> super::Operate<P> for OperateWithGuard<'_, O, G>
where
    O: super::Operate<P>,
    P: LayerPointer,
{
    #[inline]
    fn start_insert(&mut self, target: &P::Target) -> P::LayerTarget {
        self.op.start_insert(target)
    }

    fn purge<'a>(&mut self, purge: impl super::Purge<'a, P>) {
        self.op.purge(purge);
    }

    fn start_read(&mut self, pointer: &P) -> super::StartRead {
        self.op.start_read(pointer)
    }

    fn complete_read(&mut self, pointer: &P) {
        self.op.complete_read(pointer);
    }

    fn fail_insert(&mut self, target: &P::Target, value: P::LayerTarget) {
        self.op.fail_insert(target, value);
    }

    fn complete_insert(&mut self, pointer: &P) {
        self.op.complete_insert(pointer);
    }

    fn remove(&mut self, pointer: &P) -> super::Remove {
        self.op.remove(pointer)
    }
}
