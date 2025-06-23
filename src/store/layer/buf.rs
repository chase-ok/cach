use std::cell::Cell;
use std::marker::PhantomData;
use std::num::NonZero;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};

use parking_lot::{Mutex, RawMutex};
use scc::Bag;

use crate::lock::MutexGuardDetached;

use super::{BuildLayer, Layer, LayerMut, LayerPointer, Operate as _, Purge, Remove, StartRead};

pub trait RwBuffer<P: LayerPointer<LayerTarget = Self::Value>>: LayerMut<P> {
    fn start_insert(target: &P::Target) -> Self::Value;

    #[inline]
    fn fail_insert(_target: &P::Target, _value: Self::Value) {}
}

pub trait BuildRwBuffer<T>: Sized {
    type Value: 'static;

    type RwBuffer<P>: RwBuffer<P, Value = Self::Value>
    where
        P: LayerPointer<LayerTarget = Self::Value>;

    fn build_rw_buffer<P>(self) -> Self::RwBuffer<P>
    where
        P: LayerPointer<LayerTarget = Self::Value>;
}

pub struct RwBuffered<B> {
    build: B,
    capacity: usize,
    concurrency: usize,
}

impl<B> RwBuffered<B> {
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

impl<T, B> BuildLayer<T> for RwBuffered<B>
where
    B: BuildRwBuffer<T>,
{
    type Value = B::Value;

    type Layer<P>
        = RwBufferedLayer<B::RwBuffer<P>, P>
    where
        P: LayerPointer<LayerTarget = Self::Value, Target = T>;

    fn build<P>(self) -> Self::Layer<P>
    where
        P: LayerPointer<LayerTarget = Self::Value, Target = T>,
    {
        RwBufferedLayer {
            layer_and_scratch: Mutex::new((
                self.build.build_rw_buffer(),
                Vec::with_capacity(self.concurrency * self.capacity),
            )),
            bags: (0..self.concurrency).map(|_| Default::default()).collect(),
            bag_cap: self.capacity, // XX: divide by concurrency or something?
            mask: self.concurrency - 1,
        }
    }
}

pub struct RwBufferedLayer<L, P> {
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

impl<L, P> Layer<P> for RwBufferedLayer<L, P>
where
    P: LayerPointer<LayerTarget = L::Value>,
    L: RwBuffer<P>,
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

                return RwOperate::ToLayer { operate, _guard };
            }
        }

        RwOperate::ToBag {
            bag,
            _buffer: PhantomData::<L>,
        }
    }
}

// XX convert to state and add &self to start_insert
enum RwOperate<'a, B, P, O> {
    ToBag {
        bag: &'a Bag<(P, Action)>,
        _buffer: PhantomData<B>,
    },
    ToLayer {
        operate: O,
        _guard: MutexGuardDetached<'a, RawMutex>,
    },
}

impl<B, P, O> super::Operate<P> for RwOperate<'_, B, P, O>
where
    B: RwBuffer<P>,
    P: LayerPointer<LayerTarget = B::Value>,
    O: super::Operate<P>,
{
    fn start_insert(&mut self, target: &P::Target) -> P::LayerTarget {
        match self {
            Self::ToBag { .. } => B::start_insert(target),
            Self::ToLayer { operate, .. } => operate.start_insert(target),
        }
    }

    fn purge<'a>(&mut self, purge: impl Purge<'a, P>) {
        match self {
            Self::ToBag { .. } => {}
            Self::ToLayer { operate, .. } => operate.purge(purge),
        }
    }

    fn start_read(&mut self, pointer: &P) -> StartRead {
        match self {
            Self::ToBag { .. } => StartRead::Allow,
            Self::ToLayer { operate, .. } => operate.start_read(pointer),
        }
    }

    fn complete_read(&mut self, pointer: &P) {
        match self {
            Self::ToBag { bag, .. } => bag.push((pointer.clone(), Action::Read)),
            Self::ToLayer { operate, .. } => operate.complete_read(pointer),
        }
    }

    fn fail_insert(&mut self, target: &P::Target, value: P::LayerTarget) {
        match self {
            Self::ToBag { .. } => B::fail_insert(target, value),
            Self::ToLayer { operate, .. } => operate.fail_insert(target, value),
        }
    }

    fn complete_insert(&mut self, pointer: &P) {
        match self {
            Self::ToBag { bag, .. } => bag.push((pointer.clone(), Action::Insert)),
            Self::ToLayer { operate, .. } => operate.complete_insert(pointer),
        }
    }

    fn remove(&mut self, pointer: &P) -> Remove {
        match self {
            Self::ToBag { bag, .. } => {
                bag.push((pointer.clone(), Action::Remove));
                Remove::Allow
            }
            Self::ToLayer { operate, .. } => operate.remove(pointer),
        }
    }
}

pub struct ReadBufferedLayer<L, P> {
    layer: Mutex<L>,
    bags: Vec<crossbeam_utils::CachePadded<scc::Bag<P>>>,
    bag_cap: usize,
    mask: usize,
}

impl<L, P> Layer<P> for ReadBufferedLayer<L, P>
where
    P: LayerPointer<LayerTarget = L::Value>,
    L: RwBuffer<P>,
{
    type Value = L::Value;

    fn operate(&self) -> impl super::Operate<P> + '_ {
        let id = id();
        let bag = &self.bags[id & self.mask];
        if bag.len() >= self.bag_cap {
            if let Some(layer) = self.layer.try_lock() {
                // XX
                let (_guard, layer) = unsafe { MutexGuardDetached::detach_from(layer) };
                let mut operate = layer.operate_mut();

                for bag in &self.bags {
                    bag.pop_all((), |(), p| operate.complete_read(&p));
                }

                return ReadOperate { bag, state: ReadOperateState::Write { operate, _guard }}
            }
        }

        ReadOperate {
            bag,
            state: ReadOperateState::Init(|| {
                let (_guard, layer) = unsafe { MutexGuardDetached::detach_from(self.layer.lock()) };
                (layer.operate_mut(), _guard)
            }),
        }
    }
}

struct ReadOperate<'a, P, O, F> {
    bag: &'a Bag<P>,
    state: ReadOperateState<'a, O, F>,
}

enum ReadOperateState<'a, O, F> {
    None,
    Write { operate: O, _guard: MutexGuardDetached<'a, RawMutex> },
    Init(F)
}

impl<'a, O, F> ReadOperateState<'a, O, F>
where
    F: FnOnce() -> (O, MutexGuardDetached<'a, RawMutex>)
{

    fn write(&mut self) -> &mut O {
        if matches!(self, Self::Init(_)) {
            let Self::Init(f) = std::mem::replace(self, Self::None) else {
                unreachable!()
            };
            let (operate, _guard) = f();
            *self = Self::Write { operate, _guard }
        }
        let Self::Write { operate, .. } = self else {
            unreachable!()
        };
        operate
    }

    fn try_write(&mut self) -> Option<&mut O> {
        match self {
            ReadOperateState::None => unreachable!(),
            ReadOperateState::Write { operate, .. } => Some(operate),
            ReadOperateState::Init(_) => None,
        }
    }
}

impl<'a, P, O, F> super::Operate<P> for ReadOperate<'a, P, O, F>
where
    F: FnOnce() -> (O, MutexGuardDetached<'a, RawMutex>),
    P: LayerPointer,
    O: super::Operate<P>,
{
    fn start_insert(&mut self, target: &P::Target) -> P::LayerTarget {
        self.state.write().start_insert(target)
    }

    fn purge<'p>(&mut self, purge: impl Purge<'p, P>) {
        self.state.write().purge(purge)
    }

    fn start_read(&mut self, pointer: &P) -> StartRead {
        if let Some(operate) = self.state.try_write() {
            operate.start_read(pointer)
        } else {
            StartRead::Allow
        }
    }

    fn complete_read(&mut self, pointer: &P) {
        if let Some(operate) = self.state.try_write() {
            operate.complete_read(pointer);
        } else {
            self.bag.push(pointer.clone());
        }
    }

    fn fail_insert(&mut self, target: &P::Target, value: P::LayerTarget) {
        self.state.write().fail_insert(target, value);
    }

    fn complete_insert(&mut self, pointer: &P) {
        self.state.write().complete_insert(pointer);
    }

    fn remove(&mut self, pointer: &P) -> Remove {
        self.state.write().remove(pointer)
    }
}
