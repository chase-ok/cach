use std::{borrow::Borrow, hash::Hash, ops::Deref};

pub mod build;
pub mod evict;
pub mod expire;
pub mod local;
pub mod lock;
pub mod map;
pub mod sync;
pub mod time;
pub mod atomic;
mod wrap;

use crossbeam_utils::Backoff;
use time::{Clock, DefaultClock};

pub trait Cache<T: Value>: Sized {
    type Pointer: Deref<Target = T> + Clone;

    // XX name
    const PREFER_LOCKED: bool;

    fn len(&self) -> usize;

    fn entry<'c, 'k, K>(&'c self, key: &'k K) -> impl Entry<Pointer = Self::Pointer> + 'c
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq + ToOwned<Owned = T::Key>;

    fn locked_entry<'c, 'k, K>(
        &'c self,
        key: &'k K,
    ) -> LockedEntry<
        impl LockedOccupiedEntry<Pointer = Self::Pointer> + 'c,
        impl LockedVacantEntry<Pointer = Self::Pointer> + 'c,
    >
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq;

    fn insert(&self, value: T) -> Self::Pointer {
        if Self::PREFER_LOCKED {
            match self.locked_entry(value.key()) {
                LockedEntry::Occupied(o) => o.replace(value),
                LockedEntry::Vacant(v) => v.insert(value),
            }
        } else {
            self.entry(value.key()).write(value)
        }
    }

    fn upsert(&self, mut value: T, mut f: impl FnMut(&mut T, &T) -> bool) -> Self::Pointer {
        if Self::PREFER_LOCKED {
            match self.locked_entry(value.key()) {
                LockedEntry::Occupied(o) => {
                    if f(&mut value, o.value()) {
                        o.replace(value)
                    } else {
                        o.into_pointer()
                    }
                }
                LockedEntry::Vacant(v) => v.insert(value),
            }
        } else {
            self.entry(value.key()).upsert(value, f)
        }
    }

    fn or_insert(&self, value: T) -> Self::Pointer {
        if Self::PREFER_LOCKED {
            self.locked_entry(value.key()).or_insert(value)
        } else {
            self.entry(value.key())
                .write_if(value, |_value, current| current.is_none())
                .unwrap()
        }
    }

    fn or_insert_with<K>(&self, key: &K, f: impl FnOnce() -> T) -> Self::Pointer
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq + ToOwned<Owned = T::Key>,
    {
        if Self::PREFER_LOCKED {
            self.locked_entry(key).or_insert_with(f)
        } else {
            let entry = self.entry(key);
            if let Some(pointer) = entry.pointer() {
                pointer
            } else {
                let value = f();
                entry.write_if(value, |_v, current| current.is_none()).unwrap()
            }
        }
    }

    fn or_insert_default<K>(&self, key: &K) -> Self::Pointer
    where
        T: Default,
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq + ToOwned<Owned = T::Key>,
    {
        self.or_insert_with(key, Default::default)
    }

    fn remove_if<K: ?Sized>(&self, key: &K, mut f: impl FnMut(&T) -> bool) -> Option<Self::Pointer>
    where
        T::Key: Borrow<K>,
        // XX: can we remove the Borrow as redundant of ToOwned?
        K: Hash + Eq + ToOwned<Owned = T::Key>,
    {
        if Self::PREFER_LOCKED {
            match self.locked_entry(key) {
                LockedEntry::Occupied(o) if f(o.value()) => Some(o.remove()),
                _ => None,
            }
        } else {
            let mutated = self.entry(key).mutate(Mutate::Remove, |_m, current| {
                if current.is_some_and(&mut f) {
                    Mutate::Remove
                } else {
                    Mutate::None
                }
            });
            match mutated {
                Mutated::Removed(p) => Some(p),
                _ => None,
            }
        }
    }

    fn remove<K: ?Sized>(&self, key: &K) -> Option<Self::Pointer>
    where
        T::Key: Borrow<K>,
        K: Hash + Eq + ToOwned<Owned = T::Key>,
    {
        self.remove_if(key, |_| true)
    }

    fn get<K: ?Sized>(&self, key: &K) -> Option<Self::Pointer>
    where
        T::Key: Borrow<K>,
        K: Hash + Eq + ToOwned<Owned = T::Key>,
    {
        self.entry(key).pointer()
    }
}

pub enum LockedEntry<O, V> {
    Occupied(O),
    Vacant(V),
}

pub trait LockedOccupiedEntry: Sized {
    type Pointer: Deref;

    fn value(&self) -> &<Self::Pointer as Deref>::Target;

    fn pointer(&self) -> Self::Pointer;

    fn into_pointer(self) -> Self::Pointer {
        self.pointer()
    }

    fn replace(self, value: <Self::Pointer as Deref>::Target) -> Self::Pointer
    where
        <Self::Pointer as Deref>::Target: Sized;

    fn remove(self) -> Self::Pointer;
}

pub trait LockedVacantEntry {
    type Pointer: Deref;

    fn insert(self, value: <Self::Pointer as Deref>::Target) -> Self::Pointer
    where
        <Self::Pointer as Deref>::Target: Sized;
}

impl<O: LockedOccupiedEntry, V: LockedVacantEntry<Pointer = O::Pointer>> LockedEntry<O, V> {
    pub fn or_insert_with(self, f: impl FnOnce() -> <O::Pointer as Deref>::Target) -> O::Pointer
    where
        <O::Pointer as Deref>::Target: Sized,
    {
        match self {
            LockedEntry::Occupied(o) => o.into_pointer(),
            LockedEntry::Vacant(v) => v.insert(f()),
        }
    }

    pub fn or_insert(self, value: <O::Pointer as Deref>::Target) -> O::Pointer
    where
        <O::Pointer as Deref>::Target: Sized,
    {
        self.or_insert_with(|| value)
    }

    pub fn or_insert_default(self) -> O::Pointer
    where
        <O::Pointer as Deref>::Target: Default,
    {
        self.or_insert_with(Default::default)
    }
}

macro_rules! target {
    ($t:ty) => {
        <<$t>::Pointer as Deref>::Target
    };
}
pub(crate) use target;

pub trait Entry: Sized 
where 
    <Self::Pointer as Deref>::Target: Value
{
    type Pointer: Deref;

    fn value(&self) -> Option<&target!(Self)>;

    fn pointer(&self) -> Option<Self::Pointer>;

    fn key(&self) -> &<target!(Self) as Value>::Key;

    fn reload(&mut self)
    where
        target!(Self): Sized
    {
        let _ = self.try_mutate(Mutate::None);
    }

    fn try_mutate(
        &mut self,
        mutate: Mutate<target!(Self)>,
    ) -> Result<Mutated<Self::Pointer>, Mutate<target!(Self)>>
    where
        target!(Self): Sized;

    fn mutate(
        mut self,
        initial: Mutate<target!(Self)>,
        mut f: impl FnMut(Mutate<target!(Self)>, Option<&target!(Self)>) -> Mutate<target!(Self)>,
    ) -> Mutated<Self::Pointer>
    where
        target!(Self): Sized,
    {
        let backoff = Backoff::new();
        let mut attempt = initial;
        loop {
            match self.try_mutate(f(attempt, self.value())) {
                Ok(m) => return m,
                Err(next) => {
                    attempt = next;
                    backoff.spin();
                }
            }
        }
    }

    fn write_if(
        self,
        value: target!(Self),
        mut f: impl FnMut(&target!(Self), Option<&target!(Self)>) -> bool,
    ) -> Option<Self::Pointer>
    where
        target!(Self): Sized,
    {
        let mutated = self.mutate(Mutate::Write(value), |mutate, current| match mutate {
            Mutate::Write(value) if f(&value, current) => Mutate::Write(value),
            Mutate::Write(_) => Mutate::None,
            _ => unreachable!(),
        });

        match mutated {
            Mutated::None(p) => p,
            Mutated::Inserted(p) => Some(p),
            Mutated::Replaced { after, .. } => Some(after),
            _ => unreachable!(),
        }
    }

    fn write(self, value: target!(Self)) -> Self::Pointer
    where
        target!(Self): Sized,
    {
        match self.mutate(Mutate::Write(value), |mutate, _current| mutate) {
            Mutated::Inserted(p) => p,
            Mutated::Replaced { after, .. } => after,
            _ => unreachable!(),
        }
    }

    fn upsert(
        mut self,
        mut value: target!(Self),
        mut f: impl FnMut(&mut target!(Self), &target!(Self)) -> bool,
    ) -> Self::Pointer
    where
        target!(Self): Sized,
    {
        let backoff = Backoff::new();
        loop {
            if let Some(current) = self.pointer() {
                if !f(&mut value, &current) {
                    return current;
                }
            }

            match self.try_mutate(Mutate::Write(value)) {
                Ok(Mutated::Inserted(p)) => return p,
                Ok(Mutated::Replaced { after, .. }) => return after,
                Ok(_) => unreachable!(),
                Err(Mutate::Write(failed)) => value = failed,
                Err(_) => unreachable!(),
            }

            backoff.spin();
        }
    }

    fn remove_if(self, mut f: impl FnMut(&target!(Self)) -> bool) -> Option<Self::Pointer>
    where
        target!(Self): Sized,
    {
        let mutated = self.mutate(Mutate::Remove, |_mutate, current| match current {
            Some(current) if f(current) => Mutate::Remove,
            _ => Mutate::None,
        });
        match mutated {
            Mutated::Removed(p) => Some(p),
            Mutated::None(_) => None,
            _ => unreachable!(),
        }
    }

    fn remove(self) -> Option<Self::Pointer>
    where
        target!(Self): Sized,
    {
        self.remove_if(|_v| true)
    }
}

pub enum Mutate<T> {
    None,
    Write(T),
    Remove,
}

pub enum Mutated<P> {
    None(Option<P>),
    Inserted(P),
    Removed(P),
    Replaced { before: P, after: P },
}

pub trait Value {
    type Key: Hash + Eq + Clone;

    fn key(&self) -> &Self::Key;
}

#[test]
fn test() {
    use build::{BuildCache as _, BuildCacheExt as _};
    use evict::lru::EvictLeastRecentlyUsed;
    use std::time::Instant;

    struct Test {
        key: String,
        expire: Instant,
    }

    impl Value for Test {
        type Key = String;

        fn key(&self) -> &Self::Key {
            &self.key
        }
    }

    impl expire::ExpireAt for Test {
        fn expire_at(&self) -> Instant {
            self.expire
        }
    }

    let cache = sync::SyncCacheBuilder::new()
        .evict(evict::lri::EvictExpiredLeastRecentlyInserted::default())
        .expire_at()
        .build();
    // .expire_intrusive();
    // .layer(expire::ExpireIntrusive);
    // .expire_intrusive();
    cache.insert(Test {
        key: "abc".into(),
        expire: Instant::now(),
    });
}
