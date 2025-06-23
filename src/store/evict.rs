use scc::{LinkedList, Queue};

use crate::store::layer::{BuildLayer, BuildLayerMut, Layer, LayerMut, LayerPointer, Operate};

use super::layer::Purge;

mod key;


#[derive(Debug, Clone)]
pub struct BuildLeastRecentlyWrittenLayer {
    capacity: usize
}

impl<T> BuildLayerMut<T> for BuildLeastRecentlyWrittenLayer {
    type Value = ();

    type LayerMut<P> = LeastRecentlyWrittenLayer<P>
    where
        P: LayerPointer<LayerTarget = Self::Value, Target = T>;

    fn build_mut<P>(self) -> Self::LayerMut<P>
    where
        P: LayerPointer<LayerTarget = Self::Value, Target = T>
    {
        LeastRecentlyWrittenLayer { capacity: self.capacity, list: Queue::default() }
    }
}

pub struct LeastRecentlyWrittenLayer<P> {
    capacity: usize,
    list: Queue<P>,
}

impl<P> LayerMut<P> for LeastRecentlyWrittenLayer<P>
where
    P: LayerPointer<LayerTarget = ()>,
{
    type Value = ();

    fn operate_mut(&mut self) -> impl Operate<P> + '_ {
        struct Op;

        impl<P> Operate<P> for Op
        where
            P: LayerPointer<LayerTarget = ()>,
        {
            fn start_insert(&mut self, _target: &P::Target) {
            }

            fn purge<'a>(&mut self, purge: impl Purge<'a, P>) {

            }

        }

        Op
    }
}