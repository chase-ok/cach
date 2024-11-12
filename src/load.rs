use std::{borrow::Borrow, future::Future, hash::Hash};

use crate::{Cache, Value};

mod dedup;
pub use dedup::DedupLoadIntrusive;


pub trait AsyncLoad<T: Value> {
    type Output;

    fn load<K>(&self, key: &K) -> impl Future<Output = Self::Output> + Send
    where
        K: ?Sized + ToOwned<Owned = T::Key> + Hash + Eq,
        T::Key: Borrow<K>;
}

pub trait AsyncTryLoad<T: Value>: AsyncLoad<T, Output = Result<Option<T>, Self::Error>> {
    type Error: Send;
}

impl<T, L, E> AsyncTryLoad<T> for L
where
    T: Value,
    L: AsyncLoad<T, Output = Result<Option<T>, E>>,
    E: Send,
{
    type Error = E;
}


pub trait AsyncLoadCache<T: Value + Send>: Cache<T> + AsyncLoad<T, Output = Self::Pointer> {}

impl<T, C> AsyncLoadCache<T> for C
where
    T: Value + Send,
    C: Cache<T> + AsyncLoad<T, Output = C::Pointer>,
{
}