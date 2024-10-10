use std::{borrow::Borrow, hash::Hash, ops::Deref};

use crate::{
    time::ExpiryTime, Cache, wrap::CacheWrapper, Clock, DefaultClock, OccupiedEntry, Value,
};

pub trait Expire {
    fn is_expired(&self) -> bool;
}

pub(crate) struct ExpireCache<C>(pub C);

impl<T, C> Cache<T> for ExpireCache<C>
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
                if occupied.value().is_expired() {
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
