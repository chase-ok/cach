use std::{any::Any, hash::BuildHasher, marker::PhantomData, ops::Deref, sync::Arc};

use arc_swap::{ArcSwap, ArcSwapAny};
use crossbeam_utils::CachePadded;
use hashbrown::{hash_table, DefaultHashBuilder, HashTable};
use parking_lot::RwLock;

use crate::store::{layer::{AndThen, BuildLayer, InsertShared, Layer, LayerDeref, Lock, NoneLayer}, BuildStore, HashValue, Store};


pub struct Builder<S = DefaultHashBuilder> {
    hash_builder: S,
}

impl<S: BuildHasher> BuildStore for Builder<S> {

    type Store<T, L> = SyncStore<T, L::Layer<ErasedPointer<T, L::Value>, DerefErasedLayer<T, L::Value>>, L::Value, S>
    where
        T: 'static + HashValue,
        L: BuildLayer<T>;

    fn build_store_with_layer<T, L>(self, layer: L) -> Self::Store<T, L>
    where
        T: 'static + HashValue,
        L: BuildLayer<T>
    {
        todo!()
    }

}

pub struct ErasedPointer<T, V> {
    inner: Arc<dyn Any + Send + Sync + 'static>,
    _marker: PhantomData<(T, V)>,
}

impl<T, V> Clone for ErasedPointer<T, V> {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone(), _marker: PhantomData }
    }
}

impl<T: 'static, V: 'static> Deref for ErasedPointer<T, V> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        if let Some(pointer) = self.inner.downcast_ref::<(T, V)>() {
            &pointer.0
        } else {
            panic!()
        }
    }
}

pub struct DerefErasedLayer<T, V>(PhantomData<(T, V)>);

impl<T, V> LayerDeref<ErasedPointer<T, V>, V> for DerefErasedLayer<T, V> {
    fn deref(pointer: &ErasedPointer<T, V>) -> &V {
        todo!()
    }
}

pub struct SyncPointer<T>(Arc<T>);

pub struct SyncStore<T, L, Lv, S> {
    shards: Vec<CachePadded<RwLock<Shard<T>>>>,
    layer: L,
    _layer_value: PhantomData<Lv>,
    hash_builder: S,
    mask: usize,
}

struct Shard<T> {
    values: HashTable<ArcSwap<T>>,
}

impl<T: HashValue + 'static, L, Lv: 'static, S: BuildHasher> Store<T> for SyncStore<T, L, Lv, S>
where
    L: Layer<ErasedPointer<T, Lv>, DerefErasedLayer<T, Lv>, Value = Lv>,
{
    type Pointer = Arc<T>;

    fn len(&self) -> usize {
        todo!()
    }

    fn iter(&self) -> impl Iterator<Item = Self::Pointer> {
        std::iter::empty()
    }

    fn extract_if(
        &self,
        f: impl FnMut(&Self::Pointer) -> bool,
    ) -> impl Iterator<Item = Self::Pointer> {
        std::iter::empty()
    }

    fn insert(&self, value: T) -> Self::Pointer {
        let key = value.key();
        // let (hash, shard) = self.hash_and_shard(key);
        let (hash, shard) = (0u64, 0usize);

        if L::insert_lock() == Lock::Shared {
            let shard = self.shards[shard].read();
            match L::start_insert_shared(&self.layer, &value){
                InsertShared::Allow => {
                    let layer_value = self.layer.create_insert_shared_value(&value);

                    if let Some(swap) = shard.values.find(hash, |s| s.load().key() == key)
                    {
                        swap.store(value.clone());
                    }

                    self.layer.fail_insert_shared(&value, layer_value);
                }
                _ => { }
            }
        } else {
        }

        let value = Arc::new(value);

        if let Some(swap) = self.shards[shard]
            .read()
            .values
            .find(hash, |s| s.load().key() == key)
        {
            swap.store(value.clone());
        } else {
            let mut shard = self.shards[shard].write();
            match shard.values.entry(
                hash,
                |s| s.load().key() == key,
                |s| self.hash_builder.hash_one(s.load().key()),
            ) {
                hash_table::Entry::Occupied(occupied) => {
                    occupied.get().store(value.clone());
                }
                hash_table::Entry::Vacant(vacant) => {
                    vacant.insert(ArcSwapAny::from(value.clone()));
                }
            }
        }

        value
    }

    fn or_insert(&self, value: T) -> Self::Pointer {
        todo!()
    }

    fn remove(&self, value: &T) -> Option<Self::Pointer> {
        todo!()
    }
}