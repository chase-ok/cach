use std::ops::{Deref, DerefMut};

use crossbeam_utils::CachePadded;
use parking_lot::RwLock;

pub struct ShardedRwLock<T> {
    shards: Vec<CachePadded<RwLock<T>>>,
}

impl<T> ShardedRwLock<T> {
    pub fn len(&self) -> usize {
        self.shards.len()
    }

    pub fn read(&self, shard: usize) -> impl Deref<Target = T> {
        self.shards[shard].read()
    }

    pub fn write(&self, shard: usize) -> impl DerefMut<Target = T> {
        self.shards[shard].write()
    }

    pub fn operate<O: Op<T>>(&self, shard: usize, mut op: O) -> O::Output {
        if let Some(read) = op.read() {
            if let Some(output) = read(&self.read(shard)) {
                return output;
            }
        }

        op.write(&mut self.write(shard))
    }
}

pub trait Op<T> {
    type Output;

    fn read(&mut self) -> Option<impl FnOnce(&T) -> Option<Self::Output>>;

    fn write(&mut self, value: &mut T) -> Self::Output;

    fn and<F>(self, f: F) -> impl Op<T, Output = (Self::Output, F::Output)>
    where
        F: Op<T>,
        Self: Sized,
    {
        struct And<A, B>(A, B);

        impl<T, A, B> Op<T> for And<A, B>
        where
            A: Op<T>,
            B: Op<T>,
        {
            type Output = (A::Output, B::Output);

            #[inline]
            fn read(&mut self) -> Option<impl FnOnce(&T) -> Option<Self::Output>> {
                match (self.0.read(), self.1.read()) {
                    (Some(a), Some(b)) => Some(generalize(move |value| {
                        if let Some(a) = a(value) {
                            if let Some(b) = b(value) {
                                return Some((a, b));
                            }
                        }
                        None
                    })),
                    _ => None,
                }
            }

            fn write(&mut self, value: &mut T) -> Self::Output {
                (self.0.write(value), self.1.write(value))
            }
        }

        And(self, f)
    }
}

pub struct ReadOp<F>(F);

impl<F> ReadOp<F> {
    pub fn new(f: F) -> Self {
        Self(f)
    }
}

impl<F, T, R> Op<T> for ReadOp<F>
where
    F: FnMut(&T) -> R,
{
    type Output = R;

    #[inline]
    fn read(&mut self) -> Option<impl FnOnce(&T) -> Option<Self::Output>> {
        Some(generalize(|v| Some((self.0)(v))))
    }

    #[inline]
    fn write(&mut self, value: &mut T) -> Self::Output {
        (self.0)(value)
    }
}

pub struct WriteOp<F>(F);

impl<F> WriteOp<F> {
    #[inline]
    pub fn new(f: F) -> Self {
        Self(f)
    }
}

impl<F, T, R> Op<T> for WriteOp<F>
where
    F: FnMut(&mut T) -> R,
{
    type Output = R;

    #[inline]
    fn read(&mut self) -> Option<impl FnOnce(&T) -> Option<Self::Output>> {
        None::<fn(&T) -> Option<R>>
    }

    #[inline]
    fn write(&mut self, value: &mut T) -> Self::Output {
        (self.0)(value)
    }
}

pub struct TryReadThenWriteOp<R, W>(R, W);

impl<R, W> TryReadThenWriteOp<R, W> {
    #[inline]
    pub fn new(read: R, write: W) -> Self {
        Self(read, write)
    }
}

impl<R, W, T, S> Op<T> for TryReadThenWriteOp<R, W>
where
    R: FnMut(&T) -> Option<S>,
    W: FnMut(&mut T) -> S,
{
    type Output = S;

    #[inline]
    fn read(&mut self) -> Option<impl FnOnce(&T) -> Option<Self::Output>> {
        Some(&mut self.0)
    }

    #[inline]
    fn write(&mut self, value: &mut T) -> Self::Output {
        (self.1)(value)
    }
}

// XX annoying closure lifetimes
#[inline]
fn generalize<F: FnOnce(&T) -> R, T, R>(f: F) -> impl FnOnce(&T) -> R {
    move |v| f(v)
}
