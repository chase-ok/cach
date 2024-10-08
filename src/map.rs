use std::borrow::Borrow;
use std::marker::PhantomData;
use std::hash::Hash;
use std::ops::Deref;

use crate::Cache;


pub struct MapCache<K, V, C> {
    cache: C,
    _entry: PhantomData<(K, V)>,
}

impl<K, V, C> MapCache<K, V, C> {
    pub fn new(cache: C) -> Self {
        Self {
            cache,
            _entry: PhantomData,
        }
    }
}

pub struct MapEntry<K, V>(K, V);

impl<K: Eq + Hash, V> crate::Value for MapEntry<K, V> {
    type Key = K;

    fn key(&self) -> &Self::Key {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub struct MapPointer<P>(P);

impl<K, V, P: Deref<Target = MapEntry<K, V>>> Deref for MapPointer<P> {
    type Target = V;

    fn deref(&self) -> &Self::Target {
        &self.0.1
    }
}

impl<K: Eq + Hash, V, C: Cache<MapEntry<K, V>>> MapCache<K, V, C> {
    // XX good to override if can avoid write lock
    fn get<Q: ?Sized>(&self, key: &Q) -> Option<MapPointer<C::Pointer>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq,
    {
        self.cache.get(key).map(MapPointer)
    }
}



