use std::{marker::PhantomData, ops::Deref};

use ref_cast::RefCast;
use stable_deref_trait::{CloneStableDeref, StableDeref};

use crate::store::layer::{LayerPointer, Remove, StartRead};

use super::{BuildLayer, BuildLayerMut, Purge};

pub struct AndThen<B0, B1> {
    build_0: B0,
    build_1: B1,
}

impl<B0, B1> AndThen<B0, B1> {
    pub fn new(build_0: B0, build_1: B1) -> Self {
        Self { build_0, build_1 }
    }
}

impl<T, B0, B1> BuildLayer<T> for AndThen<B0, B1>
where
    B0: BuildLayer<T>,
    B1: BuildLayer<T>,
{
    type Value = AndThenValue<B0::Value, B1::Value>;

    type Layer<P>
        = AndThenLayer<
        B0::Layer<AndThenPointer0<P, B0::Value, B1::Value>>,
        B1::Layer<AndThenPointer1<P, B0::Value, B1::Value>>,
        B0::Value,
        B1::Value,
    >
    where
        P: LayerPointer<LayerTarget = Self::Value>;

    fn build<P>(self) -> Self::Layer<P>
    where
        P: LayerPointer<LayerTarget = Self::Value>,
    {
        AndThenLayer {
            layer_0: self.build_0.build(),
            layer_1: self.build_1.build(),
            _marker: PhantomData,
        }
    }
}

impl<T, B0, B1> BuildLayerMut<T> for AndThen<B0, B1>
where
    B0: BuildLayerMut<T>,
    B1: BuildLayerMut<T>,
{
    type Value = AndThenValue<B0::Value, B1::Value>;

    type LayerMut<P>
        = AndThenLayer<
        B0::LayerMut<AndThenPointer0<P, B0::Value, B1::Value>>,
        B1::LayerMut<AndThenPointer1<P, B0::Value, B1::Value>>,
        B0::Value,
        B1::Value,
    >
    where
        P: LayerPointer<LayerTarget = Self::Value>;

    fn build_mut<P>(self) -> Self::LayerMut<P>
    where
        P: LayerPointer<LayerTarget = Self::Value>,
    {
        AndThenLayer {
            layer_0: self.build_0.build_mut(),
            layer_1: self.build_1.build_mut(),
            _marker: PhantomData,
        }
    }
}

pub struct AndThenLayer<L0, L1, V0, V1> {
    layer_0: L0,
    layer_1: L1,
    _marker: PhantomData<(V0, V1)>,
}
pub struct AndThenValue<V0, V1>(V0, V1);

impl<P, L0, L1, V0, V1> super::Layer<P> for AndThenLayer<L0, L1, V0, V1>
where
    P: super::LayerPointer<LayerTarget = AndThenValue<V0, V1>>,
    L0: super::Layer<AndThenPointer0<P, V0, V1>, Value = V0>,
    L1: super::Layer<AndThenPointer1<P, V0, V1>, Value = V1>,
    V0: 'static,
    V1: 'static,
{
    type Value = AndThenValue<V0, V1>;

    fn operate(&self) -> impl super::Operate<P> + '_ {
        Operate {
            op_0: self.layer_0.operate(),
            op_1: self.layer_1.operate(),
        }
    }
}

impl<P, L0, L1, V0, V1> super::LayerMut<P> for AndThenLayer<L0, L1, V0, V1>
where
    P: super::LayerPointer<LayerTarget = AndThenValue<V0, V1>>,
    L0: super::LayerMut<AndThenPointer0<P, V0, V1>, Value = V0>,
    L1: super::LayerMut<AndThenPointer1<P, V0, V1>, Value = V1>,
    V0: 'static,
    V1: 'static,
{
    type Value = AndThenValue<V0, V1>;

    fn operate_mut(&mut self) -> impl super::Operate<P> + '_ {
        Operate {
            op_0: self.layer_0.operate_mut(),
            op_1: self.layer_1.operate_mut(),
        }
    }
}

#[derive(RefCast)]
#[repr(transparent)]
pub struct AndThenPointer0<P, V0, V1> {
    pointer: P,
    _marker: PhantomData<(V0, V1)>,
}

impl<P, V0, V1> LayerPointer for AndThenPointer0<P, V0, V1>
where
    P: LayerPointer<LayerTarget = AndThenValue<V0, V1>>,
{
    type LayerTarget = V0;

    fn layer(&self) -> &Self::LayerTarget {
        &self.pointer.layer().0
    }
}

impl<P: Clone, V0, V1> Clone for AndThenPointer0<P, V0, V1>
where
    P: Clone,
{
    fn clone(&self) -> Self {
        Self {
            pointer: self.pointer.clone(),
            _marker: PhantomData,
        }
    }
}

impl<P: Deref, V0, V1> Deref for AndThenPointer0<P, V0, V1> {
    type Target = P::Target;

    fn deref(&self) -> &Self::Target {
        &*self.pointer
    }
}

unsafe impl<P: StableDeref, V0, V1> StableDeref for AndThenPointer0<P, V0, V1> {}
unsafe impl<P: CloneStableDeref, V0, V1> CloneStableDeref for AndThenPointer0<P, V0, V1> {}

#[derive(RefCast)]
#[repr(transparent)]
pub struct AndThenPointer1<P, V0, V1> {
    pointer: P,
    _marker: PhantomData<(V0, V1)>,
}

impl<P, V0, V1> LayerPointer for AndThenPointer1<P, V0, V1>
where
    P: LayerPointer<LayerTarget = AndThenValue<V0, V1>>,
{
    type LayerTarget = V1;

    fn layer(&self) -> &Self::LayerTarget {
        &self.pointer.layer().1
    }
}

impl<P: Clone, V0, V1> Clone for AndThenPointer1<P, V0, V1>
where
    P: Clone,
{
    fn clone(&self) -> Self {
        Self {
            pointer: self.pointer.clone(),
            _marker: PhantomData,
        }
    }
}

impl<P: Deref, V0, V1> Deref for AndThenPointer1<P, V0, V1> {
    type Target = P::Target;

    fn deref(&self) -> &Self::Target {
        &*self.pointer
    }
}

unsafe impl<P: StableDeref, V0, V1> StableDeref for AndThenPointer1<P, V0, V1> {}
unsafe impl<P: CloneStableDeref, V0, V1> CloneStableDeref for AndThenPointer1<P, V0, V1> {}

struct Operate<O0, O1> {
    op_0: O0,
    op_1: O1,
}

impl<P, O0, O1, V0, V1> super::Operate<P> for Operate<O0, O1>
where
    P: LayerPointer<LayerTarget = AndThenValue<V0, V1>>,
    O0: super::Operate<AndThenPointer0<P, V0, V1>>,
    O1: super::Operate<AndThenPointer1<P, V0, V1>>,
{
    fn start_insert(&mut self, target: &P::Target) -> P::LayerTarget {
        AndThenValue(
            self.op_0.start_insert(target),
            self.op_1.start_insert(target),
        )
    }

    fn purge<'a>(&mut self, mut purge: impl Purge<'a, P>) {
        struct Purge0<'a, U, O1> {
            purge: &'a mut U,
            op_1: &'a mut O1,
        }

        impl<'a, 'p, U, O1, P, V0, V1> Purge<'p, AndThenPointer0<P, V0, V1>> for Purge0<'a, U, O1>
        where
            U: Purge<'p, P>,
            O1: super::Operate<AndThenPointer1<P, V0, V1>>,
            P: LayerPointer<LayerTarget = AndThenValue<V0, V1>>,
        {
            fn try_remove(&mut self, pointer: &AndThenPointer0<P, V0, V1>) -> Result<(), ()> {
                self.purge.try_remove(&pointer.pointer).inspect(|_| {
                    let _ = self
                        .op_1
                        .remove(AndThenPointer1::ref_cast(&pointer.pointer));
                })
            }
        }

        self.op_0.purge(Purge0 {
            purge: &mut purge,
            op_1: &mut self.op_1,
        });

        struct Purge1<'a, U, O0> {
            purge: &'a mut U,
            op_0: &'a mut O0,
        }

        impl<'a, 'p, U, O0, P, V0, V1> Purge<'p, AndThenPointer1<P, V0, V1>> for Purge1<'a, U, O0>
        where
            U: Purge<'p, P>,
            O0: super::Operate<AndThenPointer0<P, V0, V1>>,
            P: LayerPointer<LayerTarget = AndThenValue<V0, V1>>,
        {
            fn try_remove(&mut self, pointer: &AndThenPointer1<P, V0, V1>) -> Result<(), ()> {
                self.purge.try_remove(&pointer.pointer).inspect(|_| {
                    let _ = self
                        .op_0
                        .remove(AndThenPointer0::ref_cast(&pointer.pointer));
                })
            }
        }

        self.op_1.purge(Purge1 {
            purge: &mut purge,
            op_0: &mut self.op_0,
        });
    }

    fn start_read(&mut self, pointer: &P) -> StartRead {
        match self.op_0.start_read(AndThenPointer0::ref_cast(pointer)) {
            StartRead::Allow => self.op_1.start_read(AndThenPointer1::ref_cast(pointer)),
            StartRead::Remove => StartRead::Remove,
        }
    }

    fn complete_read(&mut self, pointer: &P) {
        self.op_0.complete_read(AndThenPointer0::ref_cast(pointer));
        self.op_1.complete_read(AndThenPointer1::ref_cast(pointer));
    }

    fn fail_insert(&mut self, target: &P::Target, value: P::LayerTarget) {
        self.op_0.fail_insert(target, value.0);
        self.op_1.fail_insert(target, value.1);
    }

    fn complete_insert(&mut self, pointer: &P) {
        self.op_0
            .complete_insert(AndThenPointer0::ref_cast(pointer));
        self.op_1
            .complete_insert(AndThenPointer1::ref_cast(pointer));
    }

    fn remove(&mut self, pointer: &P) -> Remove {
        match (
            self.op_0.remove(AndThenPointer0::ref_cast(pointer)),
            self.op_1.remove(AndThenPointer1::ref_cast(pointer)),
        ) {
            (Remove::Allow, Remove::Allow) => Remove::Allow,
            _ => Remove::Hide,
        }
    }
}
