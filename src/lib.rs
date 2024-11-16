use std::{
    borrow::Borrow,
    hash::Hash,
    ops::Deref,
};

pub mod build;
pub mod evict;
pub mod expire;
pub mod local;
pub mod map;
pub mod sync;
pub mod time;
pub mod load;
mod wrap;
mod layer;

use time::{Clock, DefaultClock};

pub trait Cache<T: Value> {
    type Pointer: Deref<Target = T> + Clone;

    fn len(&self) -> usize;

    fn iter(&self) -> impl Iterator<Item = Self::Pointer>;

    fn entry<'c, 'k, K>(
        &'c self,
        key: &'k K,
    ) -> Entry<
        impl OccupiedEntry<Pointer = Self::Pointer> + 'c,
        impl VacantEntry<Pointer = Self::Pointer> + 'c,
    >
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq;
    
    fn try_entry<'c, 'k, K>(
        &'c self,
        key: &'k K,
    ) -> Option<Entry<
        impl OccupiedEntry<Pointer = Self::Pointer> + 'c,
        impl VacantEntry<Pointer = Self::Pointer> + 'c,
    >>
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq
    {
        Some(self.entry(key))
    }

    fn insert(&self, value: T) -> Self::Pointer {
        match self.entry(value.key()) {
            Entry::Occupied(o) => o.replace(value),
            Entry::Vacant(v) => v.insert(value),
        }
    }

    fn upsert(&self, value: T, f: impl FnOnce(T, &T) -> Option<T>) -> Self::Pointer {
        match self.entry(value.key()) {
            Entry::Occupied(o) => {
                if let Some(replacement) = f(value, o.value()) {
                    o.replace(replacement)
                } else {
                    o.into_pointer()
                }
            }
            Entry::Vacant(v) => v.insert(value),
        }
    }

    fn or_insert(&self, value: T) -> Self::Pointer {
        self.entry(value.key()).or_insert(value)
    }

    fn or_insert_with<K>(&self, key: &K, f: impl FnOnce() -> T) -> Self::Pointer
    where
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        self.entry(key).or_insert_with(f)
    }

    fn or_insert_default<K>(&self, key: &K) -> Self::Pointer
    where
        T: Default,
        T::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        self.or_insert_with(key, Default::default)
    }

    fn remove_if<K: ?Sized>(&self, key: &K, f: impl FnOnce(&T) -> bool) -> Option<Self::Pointer>
    where
        T::Key: Borrow<K>,
        K: Hash + Eq,
    {
        match self.entry(key) {
            Entry::Occupied(o) if f(o.value()) => Some(o.remove()),
            _ => None,
        }
    }

    fn remove<K: ?Sized>(&self, key: &K) -> Option<Self::Pointer>
    where
        T::Key: Borrow<K>,
        K: Hash + Eq,
    {
        self.remove_if(key, |_existing| true)
    }

    // XX good to override if can avoid write lock
    fn get<K: ?Sized>(&self, key: &K) -> Option<Self::Pointer>
    where
        T::Key: Borrow<K>,
        K: Hash + Eq,
    {
        match self.entry(key) {
            Entry::Occupied(o) => Some(o.into_pointer()),
            Entry::Vacant(_) => None,
        }
    }
}

#[derive(Debug)]
pub enum Entry<O, V> {
    Occupied(O),
    Vacant(V),
}

pub trait OccupiedEntry: Sized {
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

pub trait VacantEntry {
    type Pointer: Deref;

    fn insert(self, value: <Self::Pointer as Deref>::Target) -> Self::Pointer
    where
        <Self::Pointer as Deref>::Target: Sized;
}

impl<O: OccupiedEntry, V: VacantEntry<Pointer = O::Pointer>> Entry<O, V> {
    pub fn or_insert_with(self, f: impl FnOnce() -> <O::Pointer as Deref>::Target) -> O::Pointer
    where
        <O::Pointer as Deref>::Target: Sized,
    {
        match self {
            Entry::Occupied(o) => o.into_pointer(),
            Entry::Vacant(v) => v.insert(f()),
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

pub trait Value {
    type Key: ?Sized + Hash + Eq;

    fn key(&self) -> &Self::Key;
}

#[test]
fn test() {
    use build::{BuildCache};
    use std::{time::{Duration, Instant}, future::Future};
    use load::{AsyncLoad, AsyncLoadCache};

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

    struct TestSource;

    impl AsyncLoad<Test> for TestSource {
        type Output = Test;

        fn load<K>(&self, key: &K) -> impl Future<Output = Self::Output> + Send
        where
            K: ?Sized + ToOwned<Owned = String>,
        {
            let key = key.to_owned();
            async move {
                Test {
                    key,
                    expire: Instant::now(),
                }
            }
        }
    }

    let cache = BuildCache::<Test>::default()
        .expire_at()
        .build_sync();

    // let cache = sync::SyncCacheBuilder::new()
    //     .evict(evict::EvictApproximate::with_window(
    //         evict::touch::EvictLeastRecentlyTouched,
    //         Duration::from_secs(1),
    //     ))
    //     .expire_at()
    //     .build_load_dedup(TestSource);

    // async fn use_cache(cache: &impl AsyncLoadCache<Test>) {
    //     cache.insert(Test {
    //         key: "abc".into(),
    //         expire: Instant::now(),
    //     });

    //     let entry = cache.load("abc").await;
    // }
    // let _ = use_cache(&cache);
}
