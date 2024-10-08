use std::{borrow::Borrow, hash::Hash, ops::Deref};

use crate::{time::ExpiryTime, BuildCache, Cache, CachePointerFn, Clock, DefaultClock, OccupiedEntry, Value};

pub trait Expire {
    fn is_expired(&self) -> bool;
}

pub(crate) struct IntrusiveExpireCacheBuilder<B>(B);

impl<B> IntrusiveExpireCacheBuilder<B> {
    pub fn new(builder: B) -> Self {
        Self(builder)
    }
}

impl<T, B> BuildCache<T> for IntrusiveExpireCacheBuilder<B>
where
    T: Value + Expire,
    B: BuildCache<T>,
{
    fn build(self) -> impl Cache<T> {
        IntrusiveExpireCache(self.0)
    }
}


struct IntrusiveExpireCache<C>(C);

impl<T, C> Cache<T> for IntrusiveExpireCache<C>
where
    T: Value + Expire,
    C: Cache<T>,
{
    type Pointer = C::Pointer;

    fn len(&self) -> usize {
        self.0.len()
    }

    fn entry<'c, 'k, K>(
        &'c self,
        key: &'k K,
    ) -> crate::Entry<
        impl crate::OccupiedEntry<Pointer = Self::Pointer> + 'c,
        impl crate::VacantEntry<Pointer = Self::Pointer> + 'c,
    >
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        match self.0.entry(key) {
            crate::Entry::Occupied(occupied) => {
                if occupied.value().is_expired()
                {
                    crate::Entry::Occupied(occupied)
                } else {
                    crate::Entry::Vacant(Vacant(Some(VacantInner::Expired(occupied))))
                }
            }

            crate::Entry::Vacant(vacant) => {
                crate::Entry::Vacant(Vacant(Some(VacantInner::Vacant(vacant))))
            }
        }
    }
}

struct Vacant<O: crate::OccupiedEntry, V>(Option<VacantInner<O, V>>);

enum VacantInner<O, V> {
    Expired(O),
    Vacant(V),
}

impl<O: crate::OccupiedEntry, V: crate::VacantEntry<Pointer = O::Pointer>> crate::VacantEntry
    for Vacant<O, V>
{
    type Pointer = O::Pointer;

    fn insert(mut self, value: <Self::Pointer as Deref>::Target) -> Self::Pointer
    where
        <Self::Pointer as Deref>::Target: Sized,
    {
        match self.0.take().unwrap() {
            VacantInner::Expired(o) => o.replace(value),
            VacantInner::Vacant(v) => v.insert(value),
        }
    }
}

impl<O: crate::OccupiedEntry, V> Drop for Vacant<O, V> {
    fn drop(&mut self) {
        if let Some(inner) = self.0.take() {
            match inner {
                VacantInner::Expired(o) => {
                    o.remove();
                }
                _ => {}
            }
        }
    }
}

pub(crate) struct IntrusiveExpiryTimeCacheBuilder<B, C = DefaultClock> {
    builder: B,
    clock: C,
}

impl<B, C> IntrusiveExpiryTimeCacheBuilder<B, C> {
    pub fn new(builder: B, clock: C) -> Self {
        Self { builder, clock }
    }
}

impl<T, B, C> BuildCache<T> for IntrusiveExpiryTimeCacheBuilder<B, C>
where 
    T: Value + ExpiryTime,
    B: BuildCache<ExpiryTimeValue<T>>,
    C: Clock + Clone,
{
    fn build(self) -> impl Cache<T> {
        CachePointerFn::new(self.builder.intrusive_expiry(), move |pointer| ExpiryTimePointer(pointer))
    }
}

pub struct ExpiryTimeValue<T>(T);
pub struct ExpiryTimePointer<P>(P);

struct IntrusiveExpiryTimeCache<C, Clk> {
    cache: IntrusiveExpireCache<C>,
    clock: Clk,
}

impl<T, C, Clk> Cache<T> for IntrusiveExpiryTimeCache<C, Clk> 
where
    T: Value + ExpiryTime,
    C: Cache<ExpiryTimeValue<T>>,
    C: Clock + Default,
{
    type Pointer = ExpiryTimePointer<C::Pointer>;

    fn len(&self) -> usize {
        self.cache.len()
    }

    fn entry<'c, 'k, K>(
        &'c self,
        key: &'k K,
    ) -> crate::Entry<impl OccupiedEntry<Pointer = Self::Pointer> + 'c, impl crate::VacantEntry<Pointer = Self::Pointer> + 'c>
    where
        <T as Value>::Key: Borrow<K>,
        K: ?Sized + Hash + Eq 
    {
        
    }
}

