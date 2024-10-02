use hashbrown::{hash_map::DefaultHashBuilder, hash_table::Entry, HashTable};
use std::{
    borrow::Borrow,
    hash::{BuildHasher, Hash},
    ops::Deref,
    sync::{Arc, RwLock},
};

use crate::{Cache, Mutate, MutateResult, Value};

pub struct SyncCache<T, S = DefaultHashBuilder> {
    values: RwLock<HashTable<InnerEntry<T>>>,
    hash_builder: S,
}

#[derive(Debug)]
pub struct SyncEntry<T>(Arc<T>);

impl<T> Clone for SyncEntry<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Deref for SyncEntry<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

struct InnerEntry<T>(Arc<T>);

impl<T> InnerEntry<T> {
    fn externalize(&self) -> SyncEntry<T> {
        SyncEntry(Arc::clone(&self.0))
    }
}

impl<T: Value + 'static> Cache for SyncCache<T>
where
    T::Key: Hash + Eq,
{
    type Value = T;
    type Shared = SyncEntry<T>;

    fn len(&self) -> usize {
        self.values.read().unwrap().len()
    }

    fn mutate(
        &self,
        value: Self::Value,
        f: impl FnOnce(Self::Value, Option<&Self::Value>) -> Mutate<Self::Value>,
    ) -> MutateResult<Self::Shared> {
        self.mutate_inner(value, Value::key, f)
    }

    fn mutate_key<K: ?Sized>(
        &self,
        key: &K,
        f: impl FnOnce(Option<&Self::Value>) -> Mutate<Self::Value>,
    ) -> MutateResult<Self::Shared>
    where
        <Self::Value as Value>::Key: Borrow<K>,
        K: Hash + Eq,
    {
        self.mutate_inner(key, |k| k, |_key, value| f(value))
    }

    fn get<K>(&self, key: &K) -> Option<Self::Shared>
    where
        <Self::Value as Value>::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        let hash = self.hash_builder.hash_one(key);
        let values = self.values.read().unwrap();
        values
            .find(hash, |entry| entry.0.key().borrow() == key)
            .map(InnerEntry::externalize)
    }
}

impl<T: Value + 'static> SyncCache<T>
where
    T::Key: Hash + Eq,
{
    fn mutate_inner<V, K: ?Sized>(
        &self,
        target: V,
        key: impl Fn(&V) -> &K,
        f: impl FnOnce(V, Option<&T>) -> Mutate<T>,
    ) -> MutateResult<SyncEntry<T>>
    where
        T::Key: Borrow<K>,
        K: Hash + Eq,
    {
        let key = key(&target);
        let hash = self.hash_builder.hash_one(key);

        let mut values = self.values.write().unwrap();
        let entry = values.entry(
            hash,
            |entry| entry.0.key().borrow() == key,
            |entry| self.hash_builder.hash_one(entry.0.key()),
        );
        match entry {
            Entry::Occupied(mut entry) => match f(target, Some(&entry.get().0)) {
                Mutate::None => MutateResult::None(Some(entry.get().externalize())),
                Mutate::Insert(value) => {
                    let before = std::mem::replace(entry.get_mut(), InnerEntry(Arc::new(value)));
                    let after = entry.get().externalize();
                    drop(values); // release lock
                    MutateResult::Update {
                        before: Some(before.externalize()),
                        after: Some(after),
                    }
                }
                Mutate::Remove => {
                    let (removed, _entry) = entry.remove();
                    drop(values); // release lock
                    MutateResult::Update {
                        before: Some(removed.externalize()),
                        after: None,
                    }
                }
            },

            Entry::Vacant(vacant) => match f(target, None) {
                Mutate::Insert(value) => {
                    let entry = vacant.insert(InnerEntry(Arc::new(value)));
                    MutateResult::Update {
                        before: None,
                        after: Some(entry.get().externalize()),
                    }
                }
                Mutate::None | Mutate::Remove => MutateResult::None(None),
            },
        }
    }
}
