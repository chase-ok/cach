use std::{borrow::Borrow, collections::VecDeque, hash::Hash, ops::Deref, sync::Arc};

use index_list::IndexList;
use slotmap::DefaultKey;

pub mod map;
pub mod sharded;
// pub mod sync;

pub trait Cache {
    type Value: Value;
    type Shared: Deref<Target = Self::Value>;

    fn len(&self) -> usize;

    fn entry<'c, 'k, K>(
        &'c self,
        key: &'k K,
    ) -> Entry<impl OccupiedEntry<Cache = Self> + 'c, impl VacantEntry<Cache = Self> + 'c>
    where
        <Self::Value as Value>::Key: Borrow<K>,
        K: ?Sized + Hash + Eq;

    fn insert(&self, value: Self::Value) -> Self::Shared {
        match self.entry(value.key()) {
            Entry::Occupied(o) => o.replace(value),
            Entry::Vacant(v) => v.insert(value),
        }
    }

    fn upsert(
        &self,
        value: Self::Value,
        f: impl FnOnce(Self::Value, &Self::Value) -> Option<Self::Value>,
    ) -> Self::Shared {
        match self.entry(value.key()) {
            Entry::Occupied(o) => {
                if let Some(replacement) = f(value, o.value()) {
                    o.replace(replacement)
                } else {
                    o.into_shared()
                }
            }
            Entry::Vacant(v) => v.insert(value),
        }
    }

    fn or_insert(&self, value: Self::Value) -> Self::Shared {
        self.entry(value.key()).or_insert(value)
    }

    fn or_insert_with<K>(&self, key: &K, f: impl FnOnce() -> Self::Value) -> Self::Shared
    where
        <Self::Value as Value>::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        self.entry(key).or_insert_with(f)
    }

    fn or_insert_default<K>(&self, key: &K) -> Self::Shared
    where
        <Self::Value as Value>::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
        Self::Value: Default,
    {
        self.or_insert_with(key, Default::default)
    }

    fn remove_if<K: ?Sized>(
        &self,
        key: &K,
        f: impl FnOnce(&Self::Value) -> bool,
    ) -> Option<Self::Shared>
    where
        <Self::Value as Value>::Key: Borrow<K>,
        K: Hash + Eq,
    {
        match self.entry(key) {
            Entry::Occupied(o) if f(o.value()) => Some(o.remove()),
            _ => None,
        }
    }

    fn remove<K: ?Sized>(&self, key: &K) -> Option<Self::Shared>
    where
        <Self::Value as Value>::Key: Borrow<K>,
        K: Hash + Eq,
    {
        self.remove_if(key, |_existing| true)
    }

    // XX good to override if can avoid write lock
    fn get<K: ?Sized>(&self, key: &K) -> Option<Self::Shared>
    where
        <Self::Value as Value>::Key: Borrow<K>,
        K: Hash + Eq,
    {
        match self.entry(key) {
            Entry::Occupied(o) => Some(o.into_shared()),
            Entry::Vacant(_) => None,
        }
    }
}

pub enum Entry<O, V> {
    Occupied(O),
    Vacant(V),
}

pub trait OccupiedEntry: Sized {
    type Cache: Cache + ?Sized;

    fn shared(&self) -> <Self::Cache as Cache>::Shared;

    fn into_shared(self) -> <Self::Cache as Cache>::Shared {
        self.shared()
    }

    fn value(&self) -> &<Self::Cache as Cache>::Value;

    fn replace(self, value: <Self::Cache as Cache>::Value) -> <Self::Cache as Cache>::Shared;

    fn remove(self) -> <Self::Cache as Cache>::Shared;
}

pub trait VacantEntry {
    type Cache: Cache + ?Sized;

    fn insert(self, value: <Self::Cache as Cache>::Value) -> <Self::Cache as Cache>::Shared;
}

impl<O: OccupiedEntry, V: VacantEntry<Cache = O::Cache>> Entry<O, V> {
    pub fn or_insert_with(
        self,
        f: impl FnOnce() -> <O::Cache as Cache>::Value,
    ) -> <O::Cache as Cache>::Shared {
        match self {
            Entry::Occupied(o) => o.into_shared(),
            Entry::Vacant(v) => v.insert(f()),
        }
    }

    pub fn or_insert(self, value: <O::Cache as Cache>::Value) -> <O::Cache as Cache>::Shared {
        self.or_insert_with(|| value)
    }

    pub fn or_insert_default(self) -> <O::Cache as Cache>::Shared
    where
        <O::Cache as Cache>::Value: Default,
    {
        self.or_insert_with(Default::default)
    }
}

pub trait Value {
    type Key: ?Sized + Hash + Eq;

    fn key(&self) -> &Self::Key;
}

pub trait Eviction<T: Clone> {
    type Value;
    type Shard;

    fn new_shard(&mut self, capacity: usize) -> Self::Shard;

    fn new_value(&self, shard: &mut Self::Shard, construct: impl FnOnce(Self::Value) -> T) -> (T, Option<T>);
    fn touch_value(&self, shard: &mut Self::Shard, value: &Self::Value);
    fn remove_value(&self, shard: &mut Self::Shard, value: &Self::Value);
}

pub trait EvictionStrategy<R: Clone> {
    type ValueState;
    type ShardState;

    fn new_shard(&mut self, capacity: usize) -> Self::ShardState;
    fn new_value(&self, shard: &mut Self::ShardState, value_ref: R) -> (Self::ValueState, Option<R>);
    fn touch_value(&self, shard: &mut Self::ShardState, value: &Self::ValueState) -> bool;
    fn remove_value(&self, shard: &mut Self::ShardState, value: &Self::ValueState);
}

#[derive(Debug)]
pub struct NoEviction;

impl<R: Clone> EvictionStrategy<R> for NoEviction {
    type ValueState = ();
    type ShardState = ();

    fn new_shard(&mut self, capacity: usize) -> Self::ShardState {
        ()
    }

    fn new_value(&self, _shard: &mut Self::ShardState, _index: R) -> (Self::ValueState, Option<R>) {
        ((), None)
    }

    fn touch_value(&self, _shard: &mut Self::ShardState, _value: &Self::ValueState) -> bool {
        true
    }

    fn remove_value(&self, _shard: &mut Self::ShardState, _value: &Self::ValueState) {}
}

pub struct LruEviction;

impl<T: Clone> Eviction<T> for LruEviction {
    type Value = (usize, u32);
    type Shard = LruEvictionShard<T>;

    fn new_shard(&mut self, capacity: usize) -> Self::Shard {
        LruEvictionShard { 
            order: Vec::with_capacity(capacity),
            head: 0,
            tail: 0,
        }
    }

    fn new_value(&self, shard: &mut Self::Shard, construct: impl FnOnce(Self::Value) -> T) -> (T, Option<T>) {
        // let removed = if shard.order.len() == shard.order.capacity() {
        //     // shard.order.remove_first().map(|k| shard.slots.remove(k).unwrap())
        //     None
        // } else {
        //     None
        // };
        
        // if net new
        let index = shard.order.len();
        let value = construct((index, 0));
        shard.order.push(Node {
            generation: 0,
            value: Some(value.clone()),
            next: 0,
            prev: 0,
        });
        
        (value, None)
    }

    fn touch_value(&self, shard: &mut Self::Shard, value: &Self::Value) {
        let (index, generation) = *value;
        debug_assert_eq!(shard.order[index].generation, generation);
    }

    fn remove_value(&self, shard: &mut Self::Shard, value: &Self::Value) {
        let (index, generation) = *value;
        debug_assert_eq!(shard.order[index].generation, generation);
        shard.order[index].value = None;
        shard.order[index].generation += 1;

        todo!()
    }
    
}

struct LruEvictionShard<T> {
    order: Vec<Node<T>>,
    head: usize,
    tail: usize,
}

struct Node<T> {
    generation: u32,
    value: Option<T>,
    next: usize,
    prev: usize,
}