use std::convert::AsRef;
use std::ops::Deref;
use std::process::Output;
use std::{borrow::Borrow, future::Future, hash::Hash};

use crate::Cache;

mod dedup;
pub use dedup::DedupLoadIntrusive;

mod cache;

mod mirror;


pub trait AsyncLoad<T: crate::Value> {
    type Output;

    fn load<K>(&self, key: &K) -> impl Future<Output = Self::Output> + Send
    where
        K: ?Sized + ToOwned<Owned = T::Key> + Hash + Eq + Send + Sync,
        T::Key: Borrow<K> + Send + Sync + Sized;
}

pub trait AsyncTryLoad<T: crate::Value>: AsyncLoad<T, Output = Result<Option<T>, Self::Error>> {
    type Error: Send;
}

impl<T, L, E> AsyncTryLoad<T> for L
where
    T: crate::Value,
    L: AsyncLoad<T, Output = Result<Option<T>, E>>,
    E: Send,
{
    type Error = E;
}


pub trait AsyncLoadCache<T: crate::Value + Send>: Cache<T> + AsyncLoad<T, Output = Self::Pointer> {}

impl<T, C> AsyncLoadCache<T> for C
where
    T: crate::Value + Send,
    C: Cache<T> + AsyncLoad<T, Output = C::Pointer>,
{
}

pub trait Layer2<P> 
where 
    P: Deref,
    P::Target: crate::Value,
{
    type Incomplete: 'static;
    type Complete: 'static;

    fn insert(&self, write: impl Write<P, Self::Complete>) -> P;

    fn read(&self, pointer: &P, complete: &Self::Complete) -> ReadResult;

    fn start(&self, key: &<P::Target as crate::Value>::Key) -> Option<Self::Incomplete>;

    fn complete(&self, incomplete: &Self::Incomplete, write: impl Write<P, Self::Complete>) -> P;

    fn notify(&self, incomplete: &Self::Incomplete, pointer: &P, complete: &Self::Complete);

    fn wait(&self, incomplete: &Self::Incomplete) -> impl Future<Output = P> + Send;
}

// XX: can't fold into P to avoid type cycle

pub trait Write<P: Deref, V> {
    fn target(&self) -> &P::Target;

    fn write(self, value: V) -> P;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadResult {
    Retain,
    BackgroundRefresh,
}


pub trait Layer<T: crate::Value> {
    type Value: Value<Complete = T, Key = T::Key>;

    fn wrap_complete(&self, complete: T) -> Self::Value;

    fn force_complete(&self, complete: &T, incomplete: &Self::Value);

    fn wait_for_complete(&self, incomplete: &Self::Value) -> impl Future<Output = ()> + Send;

    fn new_incomplete(&self, key: T::Key) -> Self::Value;
}

// XX: rename to just Load
pub trait Value: crate::Value {
    type Complete: crate::Value<Key = Self::Key>;

    fn complete(&self) -> Option<&Self::Complete>;
}

struct Background<T> {
    value: T,
    
}