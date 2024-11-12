use std::{marker::PhantomData, time::Duration};

use crate::{expire, layer::{AndThen, Layer, LayerNone, Shard}, sync::{self, SyncCacheBuilder}, Cache, Value};

pub struct BuildCache<T, L = LayerNone> {
    _target: PhantomData<T>,
    layer: L,
}

impl<T, L: Default> Default for BuildCache<T, L> {
    fn default() -> Self {
        Self { _target: PhantomData, layer: L::default() }
    }
}

impl<T: Value, L> BuildCache<T, L> {
    pub fn layer<M>(self, layer: M) -> BuildCache<T, AndThen<L, M>> {
        BuildCache { _target: PhantomData, layer: AndThen::new(self.layer, layer) }
    }

    pub fn expire(self) -> BuildCache<T, AndThen<L, expire::ExpireLayer>>
    where
        Self: Sized,
        T: expire::Expire,
    {
        self.layer(expire::ExpireLayer)
    }

    pub fn expire_at(self) -> BuildCache<T, AndThen<L, expire::ExpireAtLayer>>
    where
        Self: Sized,
        T: expire::ExpireAt,
    {
        self.layer(expire::ExpireAtLayer::default())
    }

    pub fn build_custom<C>(self, cache: impl FnOnce(L) -> C) -> C 
    where 
        C: Cache<T>,
        L: Layer<C::Pointer>,
    {
        cache(self.layer)
    }

    pub fn build_sync<Lv, Ls>(self) -> sync::SyncCache<T, L::Value, L::Shard>
    where 
        L: Layer<sync::Pointer<T, Lv>, Value = Lv, Shard = Ls>,
        Ls: Shard<sync::Pointer<T, Lv>, Value = Lv>,
        T: 'static,
    {
        self.build_custom(|layer| SyncCacheBuilder::default().build_with_layer::<T, L, Lv, Ls>(layer))
    }
}



// pub trait BuildCache<T> {
//     type Cache;

//     fn build(self) -> Self::Cache;

//     fn build_load_dedup_intrusive<L>(self, load: L) -> DedupLoadIntrusive<L, Self::Cache> 
//     where 
//         Self: Sized
//     {
//         DedupLoadIntrusive::new(load, self.build())
//     }

//     fn assert_will_build(self) -> Self
//     where 
//         Self: Sized,
//         Self::Cache: Cache<T>,
//         T: Value,
//     {
//         self
//     }
// }

// pub trait Layer<C> {
//     type Cache;

//     fn layer(self, inner: C) -> Self::Cache;
// }

// pub struct BuildCacheLayer<B, L, T, S> {
//     build: B,
//     layer: L,
//     _value: PhantomData<(T, S)>,
// }

// impl<B, L, T, S> BuildCache<S> for BuildCacheLayer<B, L, T, S>
// where
//     B: BuildCache<T>,
//     L: Layer<B::Cache>,
// {
//     type Cache = L::Cache;

//     fn build(self) -> Self::Cache {
//         self.layer.layer(self.build.build())
//     }
// }

// pub trait BuildCacheExt<T>: BuildCache<T> {
//     fn layer<L, S>(self, layer: L) -> BuildCacheLayer<Self, L, T, S>
//     where
//         Self: Sized,
//         L: Layer<Self::Cache>,
//     {
//         BuildCacheLayer {
//             build: self,
//             layer,
//             _value: PhantomData,
//         }
//     }

//     // fn expire(self) -> BuildCacheLayer<Self, expire::ExpireLayer, T, T>
//     // where
//     //     Self: Sized,
//     //     T: expire::Expire,
//     // {
//     //     self.layer(expire::ExpireLayer)
//     // }

//     // fn expire_at(self) -> BuildCacheLayer<Self, expire::ExpireAtIntrusiveLayer, T, T>
//     // where
//     //     Self: Sized,
//     //     T: expire::ExpireAt,
//     // {
//     //     self.expire_at_with_clock(Default::default())
//     // }

//     // fn expire_at_with_clock<Clk>(
//     //     self,
//     //     clock: Clk,
//     // ) -> BuildCacheLayer<Self, expire::ExpireAtIntrusiveLayer<Clk>, T, T>
//     // where
//     //     Self: Sized,
//     //     T: expire::ExpireAt,
//     // {
//     //     self.layer(expire::ExpireAtIntrusiveLayer::with_clock(clock))
//     // }
// }

// impl<T, B: BuildCache<T>> BuildCacheExt<T> for B {}