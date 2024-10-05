use super::Eviction;
use super::index::{IndexList, Key};


#[derive(Debug)]
pub struct LruEviction;

impl<T: Clone> Eviction<T> for LruEviction {
    type Value = Key;
    type Shard = IndexList<T>;

    fn new_shard(&mut self, capacity: usize) -> Self::Shard {
        IndexList::with_capacity(capacity)
    }

    fn insert(
        &self,
        shard: &mut Self::Shard,
        construct: impl FnOnce(Self::Value) -> T,
    ) -> (T, Option<T>) {
        let removed = if shard.len() == shard.capacity() {
            shard.head_key().and_then(|k| shard.remove(k))
        } else {
            None
        };

        let (_key, value) = shard.insert_tail_with_key(construct);
        (value.clone(), removed)
    }

    fn touch(&self, shard: &Self::Shard, value: &Self::Value) { 
        shard.move_to_tail(*value);
    }

    fn remove(&self, shard: &mut Self::Shard, value: &Self::Value) {
        shard.remove(*value).unwrap();
    }

    fn replace(
        &self,
        shard: &mut Self::Shard,
        remove: &Self::Value,
        construct: impl FnOnce(Self::Value) -> T,
    ) -> T {
        shard.remove(*remove).unwrap();
        let (_key, value) = shard.insert_tail_with_key(construct);
        value.clone()
    }
}