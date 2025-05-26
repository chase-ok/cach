use std::{array, hash::Hash, marker::PhantomData, ops::Deref};
use equivalent::Equivalent;

use super::HashStore;

pub struct HashMap<S, K, V> {
    store: S,
    _marker: PhantomData<(K, V)>,
}

pub struct HashEntry<K, V> {
    key: K,
    value: V,
}

impl<K: Hash + Eq, V> super::HashValue for HashEntry<K, V> {
    type Key = K;

    fn key(&self) -> &Self::Key {
        &self.key
    }
}

impl<K, V> HashEntry<K, V> {
    pub fn key(&self) -> &K {
        &self.key
    }

    pub fn value(&self) -> &V {
        &self.value
    }
}

impl<S, K, V> HashMap<S, K, V> {
    pub fn new(store: S) -> Self {
        Self {
            store,
            _marker: PhantomData,
        }
    }
}

impl<S, K, V> HashMap<S, K, V>
where
    S: HashStore<HashEntry<K, V>>,
    K: Hash + Eq,
{
    pub fn len(&self) -> usize {
        self.store.len()
    }

    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    pub fn iter(
        &self,
    ) -> impl Iterator<Item = (HashKey<S::Pointer, K, V>, HashValue<S::Pointer, K, V>)> {
        self.store.iter().map(split_entry)
    }

    pub fn iter_entries(&self) -> impl Iterator<Item = S::Pointer> {
        self.store.iter()
    }

    pub fn drain(
        &self,
    ) -> impl Iterator<Item = (HashKey<S::Pointer, K, V>, HashValue<S::Pointer, K, V>)> {
        self.store.drain().map(split_entry)
    }

    pub fn drain_entries(&self) -> impl Iterator<Item = S::Pointer> {
        self.store.drain()
    }

    pub fn retain(&self, mut f: impl FnMut(&K, &V) -> bool) {
        self.store.retain(|e| f(&e.key, &e.value));
    }

    pub fn retain_entries(&self, f: impl FnMut(&S::Pointer) -> bool) {
        self.store.retain(f);
    }

    pub fn extract_if(
        &self,
        mut f: impl FnMut(&K, &V) -> bool,
    ) -> impl Iterator<Item = (HashKey<S::Pointer, K, V>, HashValue<S::Pointer, K, V>)> {
        self.store.extract_if(move |e| f(&e.key, &e.value)).map(split_entry)
    }

    pub fn extract_entries_if(
        &self,
        f: impl FnMut(&S::Pointer) -> bool,
    ) -> impl Iterator<Item = S::Pointer> {
        self.store.extract_if(f)
    }

    pub fn clear(&self) {
        self.store.clear()
    }

    pub fn insert(&self, key: K, value: V) -> S::Pointer {
        self.store.insert(HashEntry { key, value })
    }

    pub fn or_insert(&self, key: K, value: V) -> S::Pointer {
        self.store.or_insert(HashEntry { key, value })
    }

    pub fn keys(&self) -> impl Iterator<Item = HashKey<S::Pointer, K, V>> {
        self.store.iter().map(|entry| HashKey { entry, _marker: PhantomData })
    }

    pub fn values(&self) -> impl Iterator<Item = HashValue<S::Pointer, K, V>> {
        self.store.iter().map(|entry| HashValue { entry, _marker: PhantomData })
    }

    pub fn get<Q>(&self, key: &Q) -> Option<HashValue<S::Pointer, K, V>>
    where
        Q: ?Sized + Hash + Equivalent<K>
    {
        self.store.get(key).map(|entry| HashValue { entry, _marker: PhantomData })
    }

    pub fn get_many<Q, const N: usize>(&self, keys: [&Q; N]) -> [Option<HashValue<S::Pointer, K, V>>; N]
    where
        Q: ?Sized + Hash + Equivalent<K>,
    {
        array::from_fn(|i| self.get(keys[i]))
    }

    pub fn or_insert_with(&self, key: K, value: impl FnOnce() -> V) -> S::Pointer {
        self.store.or_insert_with(key, move |key| HashEntry { key, value: value() })
    }

    pub fn or_insert_with_default(&self, key: K) -> S::Pointer
    where
        V: Default,
    {
        self.or_insert_with(key, Default::default)
    }

    pub fn remove<Q>(&self, key: &Q) -> Option<HashValue<S::Pointer, K, V>>
    where
        Q: ?Sized + Hash + Equivalent<K>,
    {
        self.store.remove_key(key).map(|entry| HashValue { entry, _marker: PhantomData })
    }

    pub fn remove_entry<Q>(&self, key: &Q) -> Option<S::Pointer>
    where
        Q: ?Sized + Hash + Equivalent<K>,
    {
        self.store.remove_key(key)
    }
}

#[inline]
fn split_entry<E, K, V>(entry: E) -> (HashKey<E, K, V>, HashValue<E, K, V>)
where
    E: Clone,
{
    (
        HashKey {
            entry: entry.clone(),
            _marker: PhantomData,
        },
        HashValue {
            entry,
            _marker: PhantomData,
        },
    )
}

pub struct HashKey<E, K, V> {
    entry: E,
    _marker: PhantomData<(K, V)>,
}

impl<E: Clone, K, V> Clone for HashKey<E, K, V> {
    fn clone(&self) -> Self {
        Self {
            entry: self.entry.clone(),
            _marker: PhantomData,
        }
    }
}

impl<E: Copy, K, V> Copy for HashKey<E, K, V> {}

impl<E: Deref<Target = HashEntry<K, V>>, K, V> Deref for HashKey<E, K, V> {
    type Target = K;

    fn deref(&self) -> &Self::Target {
        &self.entry.key
    }
}

impl<E, K, V> HashKey<E, K, V> {
    pub fn into_entry(this: Self) -> E {
        this.entry
    }
}

pub struct HashValue<E, K, V> {
    entry: E,
    _marker: PhantomData<(K, V)>,
}

impl<E: Clone, K, V> Clone for HashValue<E, K, V> {
    fn clone(&self) -> Self {
        Self {
            entry: self.entry.clone(),
            _marker: PhantomData,
        }
    }
}

impl<E: Copy, K, V> Copy for HashValue<E, K, V> {}

impl<E: Deref<Target = HashEntry<K, V>>, K, V> Deref for HashValue<E, K, V> {
    type Target = V;

    fn deref(&self) -> &Self::Target {
        &self.entry.value
    }
}

impl<E, K, V> HashValue<E, K, V> {
    pub fn into_entry(this: Self) -> E {
        this.entry
    }
}
