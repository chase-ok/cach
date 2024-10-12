use std::marker::PhantomData;

use crate::expire;


pub trait BuildCache<T> {
    type Cache;

    fn build(self) -> Self::Cache;
}

pub trait Layer<C> {
    type Cache;

    fn layer(self, inner: C) -> Self::Cache;
}

pub struct BuildCacheLayer<B, L, T, S> {
    build: B,
    layer: L,
    _value: PhantomData<(T, S)>,
}

impl<B, L, T, S> BuildCache<S> for BuildCacheLayer<B, L, T, S>
where
    B: BuildCache<T>,
    L: Layer<B::Cache>,
{
    type Cache = L::Cache;

    fn build(self) -> Self::Cache {
        self.layer.layer(self.build.build())
    }
}

pub trait BuildCacheExt<T>: BuildCache<T> {
    fn layer<L, S>(self, layer: L) -> BuildCacheLayer<Self, L, T, S>
    where
        Self: Sized,
        L: Layer<Self::Cache>,
    {
        BuildCacheLayer {
            build: self,
            layer,
            _value: PhantomData,
        }
    }

    fn expire(self) -> BuildCacheLayer<Self, expire::ExpireLayer, T, T>
    where
        Self: Sized,
        T: expire::Expire,
    {
        self.layer(expire::ExpireLayer)
    }

    fn expire_at(self) -> BuildCacheLayer<Self, expire::ExpireAtLayer, T, T>
    where
        Self: Sized,
        T: expire::ExpireAt,
    {
        self.expire_at_with_clock(Default::default())
    }

    fn expire_at_with_clock<Clk>(
        self,
        clock: Clk,
    ) -> BuildCacheLayer<Self, expire::ExpireAtLayer<Clk>, T, T>
    where
        Self: Sized,
        T: expire::ExpireAt,
    {
        self.layer(expire::ExpireAtLayer::new(clock))
    }
}

impl<T, B: BuildCache<T>> BuildCacheExt<T> for B {}