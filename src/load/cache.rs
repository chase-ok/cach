use std::{borrow::Borrow, future::Future, hash::Hash, marker::PhantomData, ops::Deref};

use crate::{Cache as _, Entry, OccupiedEntry, VacantEntry, Value as _};

use super::{AsyncLoad, Layer, Value};

pub struct Cache<C, L, A> {
    cache: C,
    layer: L,
    load: A,
}

#[derive(Debug, Clone)]
pub struct Pointer<P>(P);

impl<P> Deref for Pointer<P>
where
    P: Deref,
    P::Target: Value,
{
    type Target = <P::Target as Value>::Complete;

    fn deref(&self) -> &Self::Target {
        self.0
            .complete()
            .expect("non-complete pointer made visible")
    }
}

impl<C, L, T, A> crate::Cache<T> for Cache<C, L, A>
where
    T: crate::Value + 'static,
    L: Layer<T>,
    C: crate::Cache<L::Value>,
{
    type Pointer = Pointer<C::Pointer>;

    fn len(&self) -> usize {
        self.cache.len()
    }

    fn iter(&self) -> impl Iterator<Item = Self::Pointer> {
        self.cache
            .iter()
            .filter(|p| p.complete().is_some())
            .map(Pointer)
    }

    fn entry<'c, 'k, K>(
        &'c self,
        key: &'k K,
    ) -> Entry<
        impl crate::OccupiedEntry<Pointer = Self::Pointer> + 'c,
        impl crate::VacantEntry<Pointer = Self::Pointer> + 'c,
    >
    where
        <T as crate::Value>::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        match self.cache.entry::<K>(key) {
            Entry::Occupied(inner) if inner.value().complete().is_some() => {
                Entry::Occupied(OccupiedComplete {
                    inner,
                    cache: self,
                    _marker: PhantomData::<T>,
                })
            }
            Entry::Occupied(inner) => Entry::Vacant(Vacant::Incomplete {
                inner,
                cache: self,
                _marker: PhantomData::<T>,
            }),
            Entry::Vacant(inner) => Entry::Vacant(Vacant::Missing {
                inner,
                cache: self,
                _marker: PhantomData::<T>,
            }),
        }
    }

    fn get<K: ?Sized>(&self, key: &K) -> Option<Self::Pointer>
    where
        <T as crate::Value>::Key: Borrow<K>,
        K: Hash + Eq,
    {
        self.cache
            .get(key)
            .filter(|p| p.complete().is_some())
            .map(Pointer)
    }
}

impl<C, L, A, T> AsyncLoad<T> for Cache<C, L, A>
where
    T: crate::Value + 'static,
    L: Layer<T> + Sync,
    C: crate::Cache<L::Value> + Sync,
    C::Pointer: Send,
    A: AsyncLoad<T> + Sync,
{
    type Output = Pointer<C::Pointer>;

    fn load<K>(&self, key: &K) -> impl Future<Output = Self::Output> + Send
    where
        K: ?Sized + ToOwned<Owned = <T as crate::Value>::Key> + Hash + Eq + Send + Sync,
        <T as crate::Value>::Key: Borrow<K> + Send + Sync + Sized,
    {
        enum State<P, O, V> {
            Complete(P),
            Incomplete(O),
            Missing(V),
        }

        let state = match self.cache.entry(key) {
            Entry::Occupied(occupied) => {
                if occupied.value().complete().is_some() {
                    State::Complete(occupied.into_pointer())
                } else {
                    let pointer = occupied.into_pointer();
                    State::Incomplete(async move {
                        self.layer.wait_for_complete(&pointer).await;
                        Pointer(self.cache.get::<T::Key>(pointer.key()).unwrap()) // XX
                    })
                }
            }
            Entry::Vacant(vacant) => {
                let pointer = vacant.insert(self.layer.new_incomplete(key.to_owned()));
                State::Missing(async move {
                    self.load.load(key).await;
                    Pointer(self.cache.refresh_entry(&pointer).or_insert_with(|| todo!()))
                })
            }
        };

        // XX avoid making entry sync
        async move {
            match state {
                State::Complete(pointer) => Pointer(pointer),
                State::Incomplete(f) => f.await,
                State::Missing(f) => f.await,
            }
        }
    }
}

struct OccupiedComplete<'a, C, L, A, O, T> {
    inner: O,
    cache: &'a Cache<C, L, A>,
    _marker: PhantomData<T>,
}

impl<C, L, A, O, T> OccupiedEntry for OccupiedComplete<'_, C, L, A, O, T>
where
    T: crate::Value,
    L: Layer<T>,
    C: crate::Cache<L::Value>,
    O: OccupiedEntry<Pointer = C::Pointer>,
{
    type Pointer = Pointer<C::Pointer>;

    fn value(&self) -> &T {
        self.inner.value().complete().unwrap()
    }

    fn pointer(&self) -> Self::Pointer {
        Pointer(self.inner.pointer())
    }

    fn into_pointer(self) -> Self::Pointer {
        Pointer(self.inner.into_pointer())
    }

    fn replace(self, value: T) -> Self::Pointer {
        Pointer(self.inner.replace(self.cache.layer.wrap_complete(value)))
    }

    fn remove(self) -> Self::Pointer {
        Pointer(self.inner.remove())
    }
}

enum Vacant<'a, C, L, A, O, V, T> {
    Incomplete {
        inner: O,
        cache: &'a Cache<C, L, A>,
        _marker: PhantomData<T>,
    },
    Missing {
        inner: V,
        cache: &'a Cache<C, L, A>,
        _marker: PhantomData<T>,
    },
}

impl<C, L, A, O, V, T> VacantEntry for Vacant<'_, C, L, A, O, V, T>
where
    T: crate::Value,
    L: Layer<T>,
    C: crate::Cache<L::Value>,
    O: OccupiedEntry<Pointer = C::Pointer>,
    V: VacantEntry<Pointer = C::Pointer>,
{
    type Pointer = Pointer<C::Pointer>;

    fn insert(self, value: T) -> Self::Pointer {
        match self {
            Vacant::Incomplete {
                inner,
                cache,
                _marker,
            } => {
                let incomplete = inner.pointer();
                let complete = Pointer(inner.replace(cache.layer.wrap_complete(value)));
                cache.layer.force_complete(&complete, &incomplete);
                complete
            }
            Vacant::Missing { inner, cache, .. } => {
                Pointer(inner.insert(cache.layer.wrap_complete(value)))
            }
        }
    }
}
