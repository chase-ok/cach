use std::{marker::PhantomData, ops::Deref};

pub struct MultiLayer<K, L0, L1> {
    key_fn: K,
    l0: L0,
    l1: L1,
}

impl<K, L0, L1> MultiLayer<K, L0, L1> {
    pub fn new(key_fn: K, l0: L0, l1: L1) -> Self {
        Self {
            key_fn,
            l0,
            l1,
        }
    }
}

pub enum Value<V0, V1> {
    V0(V0),
    V1(V1),
}

pub struct Shard<K, S0, S1> {
    key_fn: K,
    s0: S0,
    s1: S1,
}

impl<K, P, L0, L1> super::Layer<P> for MultiLayer<K, L0, L1>
where
    P: Deref,
    K: Clone + Fn(&P::Target) -> bool,
    L0: super::Layer<P>,
    L1: super::Layer<P>,
{
    type Value = Value<L0::Value, L1::Value>;
    type Shard = Shard<K, L0::Shard, L1::Shard>;

    fn new_shard(&self, capacity: usize) -> Self::Shard {
        Shard {
            key_fn: self.key_fn.clone(),
            s0: self.l0.new_shard(capacity),
            s1: self.l1.new_shard(capacity),
        }
    }
}

impl<K, P, S0, S1> super::Shard<P> for Shard<K, S0, S1>
where
    P: Deref,
    K: Fn(&P::Target) -> bool,
    S0: super::Shard<P>,
    S1: super::Shard<P>,
{
    type Value = Value<S0::Value, S1::Value>;

    #[inline]
    fn write<R: super::Resolve<P, Self::Value>>(
        &mut self,
        write: impl super::Write<P, Self::Value>,
    ) -> P {
        match (self.key_fn)(write.target()) {
            false => {
                struct Write<W, S0, S1> {
                    inner: W,
                    _marker: PhantomData<(S0, S1)>,
                }
        
                impl<W, P, S0, S1> super::Write<P, S0::Value> for Write<W, S0, S1>
                where
                    W: super::Write<P, Value<S0::Value, S1::Value>>,
                    P: Deref,
                    S0: super::Shard<P>,
                    S1: super::Shard<P>,
                {
                    fn target(&self) -> &<P as Deref>::Target {
                        self.inner.target()
                    }
        
                    fn remove(&mut self, pointer: &P) {
                        self.inner.remove(pointer);
                    }
        
                    fn write(self, value: S0::Value) -> P {
                        self.inner.write(Value::V0(value))
                    }
                }

                self.s0.write::<Resolve0<R, _, _>>(Write {
                    inner: write,
                    _marker: PhantomData::<(S0, S1)>,
                })
            },
            true => {
                struct Write<W, S0, S1> {
                    inner: W,
                    _marker: PhantomData<(S0, S1)>,
                }
        
                impl<W, P, S0, S1> super::Write<P, S1::Value> for Write<W, S0, S1>
                where
                    W: super::Write<P, Value<S0::Value, S1::Value>>,
                    P: Deref,
                    S0: super::Shard<P>,
                    S1: super::Shard<P>,
                {
                    fn target(&self) -> &<P as Deref>::Target {
                        self.inner.target()
                    }
        
                    fn remove(&mut self, pointer: &P) {
                        self.inner.remove(pointer);
                    }
        
                    fn write(self, value: S1::Value) -> P {
                        self.inner.write(Value::V1(value))
                    }
                }

                self.s1.write::<Resolve1<R, _, _>>(Write {
                    inner: write,
                    _marker: PhantomData::<(S0, S1)>,
                })
            },
        }
    }

    #[inline]
    fn remove<R: super::Resolve<P, Self::Value>>(&mut self, pointer: &P) {
        match R::resolve(pointer) {
            Value::V0(_) => self.s0.remove::<Resolve0<R, _, _>>(pointer),
            Value::V1(_) => self.s1.remove::<Resolve1<R, _, _>>(pointer),
        }
    }

    const READ_LOCK: super::ReadLock = S0::READ_LOCK.or(S1::READ_LOCK);

    #[inline]
    fn read_ref<R: super::Resolve<P, Self::Value>>(&self, pointer: &P) -> super::ReadResult {
        match R::resolve(pointer) {
            Value::V0(_) => self.s0.read_ref::<Resolve0<R, _, _>>(pointer),
            Value::V1(_) => self.s1.read_ref::<Resolve1<R, _, _>>(pointer),
        }
    }

    #[inline]
    fn read_mut<R: super::Resolve<P, Self::Value>>(&mut self, pointer: &P) -> super::ReadResult {
        match R::resolve(pointer) {
            Value::V0(_) => self.s0.read_mut::<Resolve0<R, _, _>>(pointer),
            Value::V1(_) => self.s1.read_mut::<Resolve1<R, _, _>>(pointer),
        }
    }

    const ITER_READ_LOCK: super::ReadLock = S0::READ_LOCK.or(S1::READ_LOCK);

    #[inline]
    fn iter_read_ref<R: super::Resolve<P, Self::Value>>(&self, pointer: &P) -> super::ReadResult {
        match R::resolve(pointer) {
            Value::V0(_) => self.s0.iter_read_ref::<Resolve0<R, _, _>>(pointer),
            Value::V1(_) => self.s1.iter_read_ref::<Resolve1<R, _, _>>(pointer),
        }
    }

    #[inline]
    fn iter_read_mut<R: super::Resolve<P, Self::Value>>(
        &mut self,
        pointer: &P,
    ) -> super::ReadResult {
        match R::resolve(pointer) {
            Value::V0(_) => self.s0.iter_read_mut::<Resolve0<R, _, _>>(pointer),
            Value::V1(_) => self.s1.iter_read_mut::<Resolve1<R, _, _>>(pointer),
        }
    }
}

struct Resolve0<R, V0, V1>(PhantomData<(R, V0, V1)>);
impl<P, R, V0, V1> super::Resolve<P, V0> for Resolve0<R, V0, V1>
where
    P: Deref,
    R: super::Resolve<P, Value<V0, V1>>,
    V0: 'static,
    V1: 'static,
{
    fn resolve(pointer: &P) -> &V0 {
        match R::resolve(pointer) {
            Value::V0(v) => v,
            Value::V1(_) => unreachable!(),
        }
    }
}

struct Resolve1<R, V0, V1>(PhantomData<(R, V0, V1)>);
impl<P, R, V0, V1> super::Resolve<P, V1> for Resolve1<R, V0, V1>
where
    P: Deref,
    R: super::Resolve<P, Value<V0, V1>>,
    V0: 'static,
    V1: 'static,
{
    fn resolve(pointer: &P) -> &V1 {
        match R::resolve(pointer) {
            Value::V0(_) => unreachable!(),
            Value::V1(v) => v,
        }
    }
}
