use super::Pointer;

mod and_then;
pub use and_then::AndThen;

mod lock;
pub use lock::{Local, Sync};

mod buf;
pub use buf::RwBuffered;

pub trait LayerPointer: Pointer {
    type LayerTarget;

    fn layer(&self) -> &Self::LayerTarget;
}

pub trait Layer<P: LayerPointer<LayerTarget = Self::Value>> {
    type Value: 'static;

    fn operate(&self) -> impl Operate<P> + '_;
}

pub trait LayerMut<P: LayerPointer<LayerTarget = Self::Value>> {
    type Value: 'static;

    fn operate_mut(&mut self) -> impl Operate<P> + '_;
}

pub trait BuildLayer<T>: Sized {
    type Value: 'static;

    type Layer<P>: Layer<P, Value = Self::Value>
    where
        P: LayerPointer<LayerTarget = Self::Value, Target = T>;

    fn build<P>(self) -> Self::Layer<P>
    where
        P: LayerPointer<LayerTarget = Self::Value, Target = T>;

    fn and_then<N>(self, next: N) -> AndThen<Self, N> {
        AndThen::new(self, next)
    }
}

pub trait BuildLayerMut<T>: Sized {
    type Value: 'static;

    type LayerMut<P>: LayerMut<P, Value = Self::Value>
    where
        P: LayerPointer<LayerTarget = Self::Value, Target = T>;

    fn build_mut<P>(self) -> Self::LayerMut<P>
    where
        P: LayerPointer<LayerTarget = Self::Value, Target = T>;

    fn and_then<N>(self, next: N) -> AndThen<Self, N>
    {
        AndThen::new(self, next)
    }

    fn local(self) -> Local<Self> {
        Local::new(self)
    }

    fn sync(self) -> Sync<Self> {
        Sync::new(self)
    }

    fn buffered(self) -> RwBuffered<Self> {
        RwBuffered::new(self)
    }

    fn buffered_with_capacity(self, capacity: usize) -> RwBuffered<Self> {
        RwBuffered::with_capacity(self, capacity)
    }
}

pub trait Operate<P: LayerPointer> {
    #[inline]
    fn purge<'a>(&mut self, _purge: impl Purge<'a, P>) {}

    #[inline]
    #[must_use]
    fn start_read(&mut self, _pointer: &P) -> StartRead {
        StartRead::Remove
    }

    #[inline]
    fn complete_read(&mut self, _pointer: &P) {}

    fn start_insert(&mut self, _target: &P::Target) -> P::LayerTarget;

    #[inline]
    fn fail_insert(&mut self, _target: &P::Target, _value: P::LayerTarget) {}

    #[inline]
    fn complete_insert(&mut self, _pointer: &P) {}

    #[inline]
    #[must_use]
    fn remove(&mut self, pointer: &P) -> Remove {
        match self.start_read(pointer) {
            StartRead::Allow => Remove::Allow,
            StartRead::Remove => Remove::Hide,
        }
    }
}

pub trait Purge<'a, P> {
    fn try_remove(&mut self, pointer: &P) -> Result<(), ()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartRead {
    Allow,
    Remove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remove {
    Allow,
    Hide,
}
