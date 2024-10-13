use std::{borrow::Borrow, hash::Hash, ops::Deref, time::Instant};

use crate::{
    build::Layer, target, time::ExpiryTime, Cache, Clock, DefaultClock, LockedOccupiedEntry, Mutate, Mutated, Value
};

pub trait Expire {
    fn is_expired(&self) -> bool;
}

pub struct ExpireLayer;

impl<C> Layer<C> for ExpireLayer {
    type Cache = ExpireCache<C>;

    fn layer(self, inner: C) -> Self::Cache {
        ExpireCache(inner)
    }
}

pub struct ExpireCache<C>(C);

impl<T, C> Cache<T> for ExpireCache<C>
where
    T: Value + Expire,
    C: Cache<T>,
{
    type Pointer = C::Pointer;

    const PREFER_LOCKED: bool = C::PREFER_LOCKED;

    fn len(&self) -> usize {
        self.0.len()
    }

    fn locked_entry<'c, 'k, K>(
        &'c self,
        key: &'k K,
    ) -> crate::LockedEntry<
        impl crate::LockedOccupiedEntry<Pointer = Self::Pointer> + 'c,
        impl crate::LockedVacantEntry<Pointer = Self::Pointer> + 'c,
    >
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        match self.0.locked_entry(key) {
            crate::LockedEntry::Occupied(occupied) => {
                if occupied.value().is_expired() {
                    crate::LockedEntry::Occupied(occupied)
                } else {
                    crate::LockedEntry::Vacant(Vacant(Some(VacantInner::Expired(occupied))))
                }
            }

            crate::LockedEntry::Vacant(vacant) => {
                crate::LockedEntry::Vacant(Vacant(Some(VacantInner::Vacant(vacant))))
            }
        }
    }

    fn entry<'c, 'k, K>(&'c self, key: &'k K) -> impl crate::Entry<Pointer = Self::Pointer> + 'c
    where
        <T as Value>::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        ExpireEntry {
            entry: self.0.entry(key), 
            expired: |v| v.is_expired()
        }
    }
}

struct Vacant<O: crate::LockedOccupiedEntry, V>(Option<VacantInner<O, V>>);

enum VacantInner<O, V> {
    Expired(O),
    Vacant(V),
}

impl<O: crate::LockedOccupiedEntry, V: crate::LockedVacantEntry<Pointer = O::Pointer>>
    crate::LockedVacantEntry for Vacant<O, V>
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

impl<O: crate::LockedOccupiedEntry, V> Drop for Vacant<O, V> {
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

struct ExpireEntry<E, F> {
    entry: Option<E>,
    expired: F,
}

impl<E, F> crate::Entry for ExpireEntry<E, F> 
where 
    E: crate::Entry, 
    <E::Pointer as Deref>::Target: Value,
    F: Fn(&target!(E)) -> bool,
{
    type Pointer = E::Pointer;

    fn value(&self) -> Option<&target!(Self)> {
        self.entry.as_ref().unwrap().value().filter(|v| (self.expired)(v))
    }

    fn pointer(&self) -> Option<Self::Pointer> {
        self.entry.as_ref().unwrap().pointer().filter(|p| (self.expired)(p))
    }

    fn key(&self) -> &<target!(Self) as Value>::Key {
        todo!()
    }

    fn try_mutate(
        &mut self,
        mutate: Mutate<target!(Self)>,
    ) -> Result<Mutated<Self::Pointer>, Mutate<target!(Self)>>
    where
        target!(Self): Sized
    {
        let mut entry = self.entry.take().unwrap();
        todo!()
        // let mut f = Some(f);
        // loop {
        //     let result = entry.try_mutate(|current| {
        //         match current {
        //             Some(value) if (self.expired)(value) => Mutate::Remove(None),
        //             current => (f.take().unwrap())(current),
        //         }
        //     });
        //     match result {
        //         Ok(mutated) if f.is_some() => return Ok(mutated),
        //         Ok(Mutated::None { .. } | Mutated::Removed { .. }) => continue,
        //         Ok(_) => unreachable!(),
        //         Err((entry, state)) => {
        //             return Err((ExpireEntry { entry: Some(entry), expired: self.expired }, state))
        //         }
        //     }
        // }
    }
}

pub trait ExpireAt {
    fn expire_at(&self) -> Instant;
}

#[derive(Debug, Default)]
pub struct ExpireAtLayer<C = DefaultClock>(C);

impl<C> ExpireAtLayer<C> {
    pub fn new(clock: C) -> Self {
        Self(clock)
    }
}

impl<C, Clk> Layer<C> for ExpireAtLayer<Clk> {
    type Cache = ExpireAtCache<C, Clk>;

    fn layer(self, inner: C) -> Self::Cache {
        ExpireAtCache {
            inner,
            clock: self.0,
        }
    }
}

pub struct ExpireAtCache<C, Clk = DefaultClock> {
    inner: C,
    clock: Clk,
}

impl<T, C, Clk> Cache<T> for ExpireAtCache<C, Clk>
where
    T: Value + ExpireAt,
    C: Cache<T>,
    Clk: Clock,
{
    type Pointer = C::Pointer;

    const PREFER_LOCKED: bool = C::PREFER_LOCKED;

    fn len(&self) -> usize {
        self.inner.len()
    }

    fn locked_entry<'c, 'k, K>(
        &'c self,
        key: &'k K,
    ) -> crate::LockedEntry<
        impl crate::LockedOccupiedEntry<Pointer = Self::Pointer> + 'c,
        impl crate::LockedVacantEntry<Pointer = Self::Pointer> + 'c,
    >
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        match self.inner.locked_entry(key) {
            crate::LockedEntry::Occupied(occupied) => {
                if occupied.value().expire_at() >= self.clock.now() {
                    crate::LockedEntry::Occupied(occupied)
                } else {
                    crate::LockedEntry::Vacant(Vacant(Some(VacantInner::Expired(occupied))))
                }
            }

            crate::LockedEntry::Vacant(vacant) => {
                crate::LockedEntry::Vacant(Vacant(Some(VacantInner::Vacant(vacant))))
            }
        }
    }

    fn entry<'c, 'k, K>(&'c self, key: &'k K) -> impl crate::Entry<Pointer = Self::Pointer> + 'c
    where
        <T as Value>::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        ExpireEntry {
            entry: self.0.entry(key), 
            expired: |v| v.is_expired()
        }
    }
}

// #[derive(Debug, Default)]
// pub struct ExpireAtIntrusive<C>(C);

// impl<T, C> Layer<ExpiryTimeValue<T, C>> for ExpireAtIntrusive<C>
// where
//     T: Value + ExpiryTime,
//     C: Clock + Clone,
// {
//     type Value = T;

//     fn layer(self, inner: impl Cache<ExpiryTimeValue<T, C>>) -> impl Cache<T> {
//         CacheWrapper::new(ExpireIntrusiveCache(inner), move |value| {
//             ExpiryTimeValue {
//                 value,
//                 clock: self.0.clone(),
//             }
//         })
//     }
// }

// impl<C: BuildCache<ExpiryTimeValue<T, Clk>>, T: Value + ExpiryTime, Clk> CacheLayer<C, T> for ExpireAtIntrusive<Clk>
// where
//     C: BuildCache<ExpiryTimeValue<T, Clk>>,
//     T: Value + ExpiryTime,
//     Clk: Clock + Clone,
// {
//     fn layer(self, inner: C) -> impl BuildCache<T> {
//         struct Build<C, Clk>(C, Clk);

//         impl<C: BuildCache<ExpiryTimeValue<T, Clk>>, T: Value + ExpiryTime, Clk> BuildCache<T> for Build<C, Clk>
//         where
//             C: BuildCache<ExpiryTimeValue<T, Clk>>,
//             T: Value + ExpiryTime,
//             Clk: Clock + Clone,
//         {
//             fn build(self) -> impl Cache<T> {
//                 CacheWrapper::new(ExpireIntrusiveCache(self.0.build()), move |value| {
//                     ExpiryTimeValue {
//                         value,
//                         clock: self.1.clone(),
//                     }
//                 })
//             }
//         }

//         Build(inner, self.0)
//     }
// }

// pub(crate) fn expire_at_intrusive<T, C>(cache: impl Cache<ExpiryTimeValue<T, C>>, clock: C) -> impl Cache<T>
// where
//     T: Value + ExpiryTime,
//     C: Clock + Clone
// {
//     CacheWrapper::new(ExpireIntrusiveCache(cache), move |value| {
//         ExpiryTimeValue {
//             value,
//             clock: clock.clone(),
//         }
//     })
// }

pub struct ExpiryTimeValue<T, C> {
    value: T,
    clock: C,
}

impl<T: Value, C> Value for ExpiryTimeValue<T, C> {
    type Key = T::Key;

    fn key(&self) -> &Self::Key {
        self.value.key()
    }
}

impl<T: ExpiryTime, C: Clock> Expire for ExpiryTimeValue<T, C> {
    fn is_expired(&self) -> bool {
        self.value
            .expiry_time()
            .is_some_and(|t| t <= self.clock.now())
    }
}

impl<T, C> Deref for ExpiryTimeValue<T, C> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}
