use std::{marker::PhantomData, ops::Deref};

use smallvec::SmallVec;

use crate::lock::{MapUpgradeReadGuard, UpgradeReadGuard, UpgradeReadGuardCell};

pub(crate) trait Layer<P: Deref> {
    type Value: 'static;
    type Shard: Shard<P, Value = Self::Value>;

    fn new_shard(&self, capacity: usize) -> Self::Shard;

    fn and_then<N>(self, next: N) -> AndThen<Self, N>
    where
        Self: Sized,
        N: Layer<P>,
    {
        AndThen(self, next)
    }
}

pub(crate) trait Shard<P: Deref>: Sized {
    type Value: 'static;

    fn write<R: Resolve<P, Self::Value>>(&mut self, write: impl Write<P, Self::Value>) -> P;

    // XX document ReadResult::Remove + ReadOnly as rare?
    const READ_LOCK_BEHAVIOR: ReadLockBehavior;

    /// If result is remove, remove() will be called after with the same pointer
    #[inline]
    fn read<'a, R: Resolve<P, Self::Value>>(
        _this: impl UpgradeReadGuard<Target = Self>,
        _pointer: &P,
    ) -> ReadResult {
        ReadResult::Retain
    }

    #[inline]
    fn iter_read<R: Resolve<P, Self::Value>>(
        this: impl UpgradeReadGuard<Target = Self>,
        pointer: &P,
    ) -> ReadResult {
        Self::read::<R>(this, pointer)
    }

    fn remove<R: Resolve<P, Self::Value>>(&mut self, pointer: &P);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadResult {
    Retain,
    Remove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadLockBehavior {
    ReadLockOnly,
    RequireWriteLock,
}

impl ReadLockBehavior {
    const fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::RequireWriteLock, _) | (_, Self::RequireWriteLock) => Self::RequireWriteLock,
            _ => Self::ReadLockOnly,
        }
    }
}

// XX: can't fold into P to avoid type cycle
pub(crate) trait Resolve<P, V> {
    fn resolve(pointer: &P) -> &V;
}

pub(crate) trait Write<P: Deref, V> {
    fn target(&self) -> &P::Target;
    fn remove(&mut self, pointer: &P);
    fn insert(self, value: V) -> P;
}

#[derive(Debug, Clone, Default)]
pub struct LayerNone;

impl<P: Deref> Layer<P> for LayerNone {
    type Value = ();
    type Shard = LayerNone;

    #[inline]
    fn new_shard(&self, _capacity: usize) -> Self::Shard {
        LayerNone
    }
}

impl<P: Deref> Shard<P> for LayerNone {
    type Value = ();

    #[inline]
    fn write<R>(&mut self, write: impl Write<P, Self::Value>) -> P {
        write.insert(())
    }

    const READ_LOCK_BEHAVIOR: ReadLockBehavior = ReadLockBehavior::ReadLockOnly;

    #[inline]
    fn remove<R>(&mut self, _pointer: &P) {}
}

pub struct AndThen<A, B>(A, B);

impl<A, B> AndThen<A, B> {
    pub(crate) fn new(a: A, b: B) -> Self {
        Self(a, b)
    }
}

pub struct AndThenShard<A, B>(A, B);

impl<P, A, B> Layer<P> for AndThen<A, B>
where
    P: Deref + Clone, // XX: move into Pointer trait?
    A: Layer<P>,
    B: Layer<P>,
{
    type Value = (A::Value, B::Value);
    type Shard = AndThenShard<A::Shard, B::Shard>;

    fn new_shard(&self, capacity: usize) -> Self::Shard {
        AndThenShard(self.0.new_shard(capacity), self.1.new_shard(capacity))
    }
}

struct ResolveA<R, A, B>(PhantomData<(R, A, B)>);
impl<P, R, A, B> Resolve<P, A> for ResolveA<R, A, B>
where
    P: Deref,
    R: Resolve<P, (A, B)>,
    A: 'static,
    B: 'static,
{
    #[inline]
    fn resolve(pointer: &P) -> &A {
        &R::resolve(pointer).0
    }
}

struct ResolveB<R, A, B>(PhantomData<(R, A, B)>);
impl<P, R, A, B> Resolve<P, B> for ResolveB<R, A, B>
where
    P: Deref,
    R: Resolve<P, (A, B)>,
    A: 'static,
    B: 'static,
{
    #[inline]
    fn resolve(pointer: &P) -> &B {
        &R::resolve(pointer).1
    }
}

impl<P, A, B> Shard<P> for AndThenShard<A, B>
where
    P: Deref + Clone,
    A: Shard<P>,
    B: Shard<P>,
{
    type Value = (A::Value, B::Value);

    #[inline]
    fn write<R: Resolve<P, (A::Value, B::Value)>>(
        &mut self,
        write: impl Write<P, Self::Value>,
    ) -> P {
        // XX remove from B direct to help optimize always starting with NoLayer
        struct WriteB<'a, P, R, W, A, B> {
            _resolve: PhantomData<R>,
            inner: W,
            a: &'a mut A,
            _b: PhantomData<B>,
            removed_by_a: &'a mut SmallVec<[P; 2]>,
        }

        impl<P, R, W, A, B> Write<P, B::Value> for WriteB<'_, P, R, W, A, B>
        where
            P: Deref + Clone,
            R: Resolve<P, (A::Value, B::Value)>,
            A: Shard<P>,
            B: Shard<P>,
            W: Write<P, (A::Value, B::Value)>,
        {
            fn target(&self) -> &<P as Deref>::Target {
                self.inner.target()
            }

            fn remove(&mut self, pointer: &P) {
                self.a.remove::<ResolveA<R, _, _>>(pointer);
                self.inner.remove(pointer);
            }

            fn insert(self, b: B::Value) -> P {
                struct WriteA<'a, P, R, W, B> {
                    _resolve: PhantomData<R>,
                    inner: W,
                    b: B,
                    // XX: move 2 to const
                    removed_by_a: &'a mut SmallVec<[P; 2]>,
                }

                impl<P, R, W, A, B> Write<P, A> for WriteA<'_, P, R, W, B>
                where
                    P: Deref + Clone,
                    R: Resolve<P, (A, B)>,
                    W: Write<P, (A, B)>,
                    A: 'static,
                    B: 'static,
                {
                    fn target(&self) -> &<P as Deref>::Target {
                        self.inner.target()
                    }

                    fn remove(&mut self, pointer: &P) {
                        self.removed_by_a.push(pointer.clone());
                        self.inner.remove(pointer);
                    }

                    fn insert(self, a: A) -> P {
                        self.inner.insert((a, self.b))
                    }
                }

                self.a.write::<ResolveA<R, _, _>>(WriteA {
                    _resolve: PhantomData::<R>,
                    inner: self.inner,
                    b,
                    removed_by_a: self.removed_by_a,
                })
            }
        }

        let mut removed_by_a = SmallVec::new();

        let written = self.1.write::<ResolveB<R, _, _>>(WriteB {
            _resolve: PhantomData::<R>,
            inner: write,
            a: &mut self.0,
            _b: PhantomData::<B>,
            removed_by_a: &mut removed_by_a,
        });

        for p in removed_by_a {
            self.1.remove::<ResolveB<R, _, _>>(&p);
        }

        written
    }

    #[inline]
    fn remove<R: Resolve<P, Self::Value>>(&mut self, pointer: &P) {
        self.0.remove::<ResolveA<R, _, _>>(pointer);
        self.1.remove::<ResolveB<R, _, _>>(pointer);
    }

    const READ_LOCK_BEHAVIOR: ReadLockBehavior = A::READ_LOCK_BEHAVIOR.and(B::READ_LOCK_BEHAVIOR);

    #[inline]
    fn read<'a, R: Resolve<P, Self::Value>>(
        this: impl UpgradeReadGuard<Target = Self>,
        pointer: &P,
    ) -> ReadResult {
        let mut this = UpgradeReadGuardCell::new(this);

        let this_a = MapUpgradeReadGuard::new(this.guard(), |s| &s.0, |s| &mut s.0);
        let result_a = A::read::<ResolveA<R, _, _>>(this_a, pointer);
        match result_a {
            ReadResult::Remove => result_a,
            ReadResult::Retain => {
                let this_b = MapUpgradeReadGuard::new(this.guard(), |s| &s.1, |s| &mut s.1);
                B::read::<ResolveB<R, _, _>>(this_b, pointer)
            }
        }
    }

    #[inline]
    fn iter_read<R: Resolve<P, Self::Value>>(
        this: impl UpgradeReadGuard<Target = Self>,
        pointer: &P,
    ) -> ReadResult {
        let mut this = UpgradeReadGuardCell::new(this);

        let this_a = MapUpgradeReadGuard::new(this.guard(), |s| &s.0, |s| &mut s.0);
        let result_a = A::iter_read::<ResolveA<R, _, _>>(this_a, pointer);
        match result_a {
            ReadResult::Remove => result_a,
            ReadResult::Retain => {
                let this_b = MapUpgradeReadGuard::new(this.guard(), |s| &s.1, |s| &mut s.1);
                B::iter_read::<ResolveB<R, _, _>>(this_b, pointer)
            }
        }
    }
}
