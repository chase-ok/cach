use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::ptr;
use std::num::NonZero;

use parking_lot::{Mutex, RawMutex};
use scc::Bag;

use crate::lock::MutexGuardDetached;

use super::{
    BuildLayer, BuildLayerMut, Layer, LayerMut, LayerPointer, Operate as _, Purge, Remove,
    StartRead,
};

pub trait RwBuffer<P: LayerPointer<LayerTarget = Self::Value>>: LayerMut<P> {
    fn start_insert(&self, target: &P::Target) -> Self::Value;

    #[inline]
    fn fail_insert(&self, _target: &P::Target, _value: Self::Value) { }
}

pub struct ReadBuffered<B> {
    build: B,
    capacity: usize,
    concurrency: usize,
}

impl<B> ReadBuffered<B> {
    pub fn new(build: B) -> Self {
        Self::with_capacity(build, 16)
    }

    pub fn with_capacity(build: B, capacity: usize) -> Self {
        Self::with_capacity_and_concurrency(
            build,
            capacity,
            std::thread::available_parallelism()
                .unwrap_or(NonZero::new(4).unwrap())
                .checked_next_power_of_two()
                .unwrap()
                .get(),
        )
    }

    pub fn with_capacity_and_concurrency(build: B, capacity: usize, concurrency: usize) -> Self {
        Self {
            build,
            capacity,
            concurrency,
        }
    }
}

impl<T, B> BuildLayer<T> for ReadBuffered<B>
where
    B: BuildLayerMut<T>,
{
    type Value = B::Value;

    type Layer<P>
        = BufferedLayer<B::LayerMut<P>, P>
    where
        P: LayerPointer<LayerTarget = Self::Value>;

    fn build<P>(self) -> Self::Layer<P>
    where
        P: LayerPointer<LayerTarget = Self::Value>,
    {
        BufferedLayer {
            layer_and_scratch: Mutex::new((
                self.build.build_mut(),
                Vec::with_capacity(self.concurrency * self.capacity),
            )),
            bags: (0..self.concurrency).map(|_| Default::default()).collect(),
            bag_cap: self.capacity, // XX: divide by concurrency or something?
            mask: self.concurrency - 1,
        }
    }
}

pub struct BufferedLayer<L, P> {
    layer_and_scratch: Mutex<(L, Vec<(P, Action)>)>,
    bags: Vec<crossbeam_utils::CachePadded<scc::Bag<(P, Action)>>>,
    bag_cap: usize,
    mask: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Action {
    // XX order matters!
    Insert,
    Read,
    Remove,
}

const ID_UNSET: usize = 0;

std::thread_local! {
    static ID: Cell<usize> = const { Cell::new(ID_UNSET) };
}

fn id() -> usize {
    let id = ID.get();
    if id != ID_UNSET {
        id
    } else {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(ID_UNSET + 1);

        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        ID.set(id);
        id
    }
}

impl<L, P> Layer<P> for BufferedLayer<L, P>
where
    P: LayerPointer<LayerTarget = L::Value>,
    L: LayerMut<P>,
{
    type Value = L::Value;

    fn operate(&self) -> impl super::Operate<P> + '_ {
        let id = id();
        let bag = &self.bags[id & self.mask];
        if bag.len() >= self.bag_cap {
            if let Some(layer_and_scratch) = self.layer_and_scratch.try_lock() {
                // XX
                let (_guard, (layer, scratch)) =
                    unsafe { MutexGuardDetached::detach_from(layer_and_scratch) };
                let mut operate = layer.operate_mut();

                debug_assert!(scratch.is_empty());
                for bag in &self.bags {
                    bag.pop_all((), |(), e| scratch.push(e));
                }
                scratch.sort_unstable_by_key(|(p, a)| (ptr::from_ref(&**p) as *const (), *a));

                for (pointer, action) in scratch.drain(..) {
                    match action {
                        Action::Insert => operate.complete_insert(&pointer),
                        Action::Read => operate.complete_read(&pointer),
                        Action::Remove => {
                            let _ = operate.remove(&pointer);
                        }
                    }
                }

                return ReadOperate::ToLayer { operate, _guard };
            }
        }

        ReadOperate::ToBag(&*bag)
    }
}

enum ReadOperate<'a, P, O> {
    ToBag(&'a Bag<(P, Action)>),
    ToLayer {
        operate: O,
        _guard: MutexGuardDetached<'a, RawMutex>,
    },
}

impl<P, O> super::Operate<P> for ReadOperate<'_, P, O>
where
    P: LayerPointer,
    O: super::Operate<P>,
{
    fn start_insert(&mut self, target: &P::Target) -> P::LayerTarget {
        match self {
            ReadOperate::ToBag(_) => todo!(),
            ReadOperate::ToLayer { operate, .. } => operate.start_insert(target),
        }
    }

    fn purge<'a>(&mut self, purge: impl Purge<'a, P>) {
        match self {
            ReadOperate::ToBag(_) => {}
            ReadOperate::ToLayer { operate, .. } => operate.purge(purge),
        }
    }

    fn start_read(&mut self, pointer: &P) -> StartRead {
        match self {
            ReadOperate::ToBag(_) => StartRead::Allow,
            ReadOperate::ToLayer { operate, .. } => operate.start_read(pointer),
        }
    }

    fn complete_read(&mut self, pointer: &P) {
        match self {
            ReadOperate::ToBag(bag) => bag.push((pointer.clone(), Action::Read)),
            ReadOperate::ToLayer { operate, .. } => operate.complete_read(pointer),
        }
    }

    fn fail_insert(&mut self, target: &P::Target, value: P::LayerTarget) {
        match self {
            ReadOperate::ToBag(bag) => todo!(),
            ReadOperate::ToLayer { operate, .. } => operate.fail_insert(target, value),
        }
    }

    fn complete_insert(&mut self, pointer: &P) {
        match self {
            ReadOperate::ToBag(bag) => bag.push((pointer.clone(), Action::Insert)),
            ReadOperate::ToLayer { operate, .. } => operate.complete_insert(pointer),
        }
    }

    fn remove(&mut self, pointer: &P) -> Remove {
        match self {
            ReadOperate::ToBag(bag) => {
                bag.push((pointer.clone(), Action::Remove));
                Remove::Allow
            }
            ReadOperate::ToLayer { operate, .. } => operate.remove(pointer),
        }
    }
}
