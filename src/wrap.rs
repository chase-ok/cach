use std::{borrow::Borrow, marker::PhantomData, ops::Deref, hash::Hash};

use crate::{Cache, Entry, OccupiedEntry, VacantEntry, Value};


pub(crate) struct CacheWrapper<C, W, F> {
    cache: C,
    wrap_fn: F,
    _wrapped: PhantomData<W>,
}

impl<C, W, F> CacheWrapper<C, W, F> {
    pub(crate) fn new(cache: C, wrap_fn: F) -> Self {
        Self {
            cache,
            wrap_fn,
            _wrapped: PhantomData,
        }
    }
}

#[derive(Clone)]
pub struct WrappedPointer<P>(P);

impl<P> Deref for WrappedPointer<P>
where
    P: Deref,
    P::Target: Deref,
{
    type Target = <P::Target as Deref>::Target;

    fn deref(&self) -> &Self::Target {
        &**self.0
    }
}

impl<T, C, W, F> Cache<T> for CacheWrapper<C, W, F>
where
    T: Value,
    C: Cache<W>,
    W: Value<Key = T::Key> + Deref<Target = T>,
    F: Fn(T) -> W,
{
    type Pointer = WrappedPointer<C::Pointer>;

    fn len(&self) -> usize {
        self.cache.len()
    }

    fn iter(&self) -> impl Iterator<Item = Self::Pointer> {
        self.cache.iter().map(WrappedPointer)
    }

    fn entry<'c, 'k, K>(
        &'c self,
        key: &'k K,
    ) -> Entry<
        impl OccupiedEntry<Pointer = Self::Pointer> + 'c,
        impl VacantEntry<Pointer = Self::Pointer> + 'c,
    >
    where
        <T as Value>::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        match self.cache.entry(key) {
            Entry::Occupied(occupied) => {
                struct Occupied<'c, O, W, F> {
                    occupied: O,
                    _wrapped: PhantomData<W>,
                    wrap_fn: &'c F,
                }

                impl<O, W, F> OccupiedEntry for Occupied<'_, O, W, F>
                where
                    O: OccupiedEntry,
                    O::Pointer: Deref<Target = W>,
                    W: Deref,
                    W::Target: Sized,
                    F: Fn(W::Target) -> W,
                {
                    type Pointer = WrappedPointer<O::Pointer>;

                    fn value(&self) -> &<Self::Pointer as Deref>::Target {
                        self.occupied.value()
                    }

                    fn pointer(&self) -> Self::Pointer {
                        WrappedPointer(self.occupied.pointer())
                    }

                    fn into_pointer(self) -> Self::Pointer {
                        WrappedPointer(self.occupied.into_pointer())
                    }

                    fn replace(self, value: <Self::Pointer as Deref>::Target) -> Self::Pointer
                    where
                        <Self::Pointer as Deref>::Target: Sized,
                    {
                        WrappedPointer(self.occupied.replace((self.wrap_fn)(value)))
                    }

                    fn remove(self) -> Self::Pointer {
                        WrappedPointer(self.occupied.remove())
                    }
                }

                Entry::Occupied(Occupied {
                    occupied,
                    _wrapped: PhantomData,
                    wrap_fn: &self.wrap_fn,
                })
            }
            Entry::Vacant(vacant) => {
                struct Vacant<'c, V, W, F> {
                    vacant: V,
                    _wrapped: PhantomData<W>,
                    wrap_fn: &'c F,
                }

                impl<V, W, F> VacantEntry for Vacant<'_, V, W, F>
                where
                    V: VacantEntry,
                    V::Pointer: Deref<Target = W>,
                    W: Deref,
                    W::Target: Sized,
                    F: Fn(W::Target) -> W,
                {
                    type Pointer = WrappedPointer<V::Pointer>;

                    fn insert(self, value: <Self::Pointer as Deref>::Target) -> Self::Pointer
                    where
                        <Self::Pointer as Deref>::Target: Sized,
                    {
                        WrappedPointer(self.vacant.insert((self.wrap_fn)(value)))
                    }
                }

                Entry::Vacant(Vacant {
                    vacant,
                    _wrapped: PhantomData,
                    wrap_fn: &self.wrap_fn,
                })
            }
        }
    }

    fn get<K: ?Sized>(&self, key: &K) -> Option<Self::Pointer>
    where
        <T as Value>::Key: Borrow<K>,
        K: Hash + Eq,
    {
        self.cache.get(key).map(WrappedPointer)
    }
}