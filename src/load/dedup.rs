use std::{
    borrow::Borrow,
    future::Future,
    hash::Hash,
    marker::PhantomData,
    ops::Deref,
    pin::Pin,
    sync::{Arc, OnceLock},
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};

use futures::{future::select, pin_mut};
use parking_lot::Mutex;
use slab::Slab;

use crate::{
    expire::{Expire, ExpireAt},
    load::AsyncLoad, Cache, Entry, OccupiedEntry, VacantEntry, Value as _,
};

#[derive(Debug)]
pub struct DedupLoadIntrusive<L, C>(Arc<DedupInner<L, C>>);

impl<L, C> DedupLoadIntrusive<L, C> {
    pub(crate) fn new(load: L, cache: C) -> Self {
        Self(Arc::new(DedupInner { load, cache }))
    }
}

impl<L, C> Clone for DedupLoadIntrusive<L, C> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

#[derive(Debug)]
struct DedupInner<L, C> {
    load: L,
    cache: C,
}

enum ValueInner<T>
where
    T: crate::Value,
    T::Key: Sized,
{
    Waiting { key: T::Key, wakers: Wakers },
    Complete(T),
}

// XX add drop type to ensure woke
type Wakers = Arc<Mutex<Option<Slab<Waker>>>>;

pub struct Value<T>(ValueInner<T>)
where
    T: crate::Value,
    T::Key: Sized;

impl<T> crate::Value for Value<T>
where
    T: crate::Value,
    T::Key: Sized,
{
    type Key = T::Key;

    fn key(&self) -> &Self::Key {
        match &self.0 {
            ValueInner::Waiting { key, .. } => key,
            ValueInner::Complete(v) => v.key(),
        }
    }
}

impl<T> Expire for Value<T>
where
    T: crate::Value + Expire,
    T::Key: Sized,
{
    fn is_expired(&self) -> bool {
        match &self.0 {
            ValueInner::Waiting { .. } => false,
            ValueInner::Complete(v) => v.is_expired(),
        }
    }
}

impl<T> ExpireAt for Value<T>
where
    T: crate::Value + ExpireAt,
    T::Key: Sized,
{
    fn expire_at(&self) -> Instant {
        static FAR_FUTURE: OnceLock<Instant> = OnceLock::new();
        match &self.0 {
            ValueInner::Waiting { .. } => *FAR_FUTURE
                .get_or_init(|| Instant::now() + Duration::from_secs(100 * 365 * 24 * 60 * 60)),
            ValueInner::Complete(v) => v.expire_at(),
        }
    }
}

#[derive(Debug)]
pub struct IntrusivePointer<P, T> {
    inner: P,
    _marker: PhantomData<T>,
}

impl<P: Clone, T> Clone for IntrusivePointer<P, T> {
    fn clone(&self) -> Self {
        Self::new(self.inner.clone())
    }
}

impl<P, T> IntrusivePointer<P, T> {
    fn new(inner: P) -> Self {
        Self {
            inner,
            _marker: PhantomData,
        }
    }
}

impl<P, T> Deref for IntrusivePointer<P, T>
where
    P: Deref<Target = Value<T>>,
    T: crate::Value,
    T::Key: Sized,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        match &self.inner.0 {
            ValueInner::Complete(p) => &p,
            _ => unreachable!(),
        }
    }
}

impl<T, L, C> AsyncLoad<T> for DedupLoadIntrusive<L, C>
where
    T: crate::Value + 'static,
    T::Key: Sized + Clone + Send,
    L: AsyncLoad<T, Output = T> + Send + Sync + 'static,
    C: Cache<Value<T>> + Send + Sync + 'static,
    C::Pointer: Send + Sync,
{
    type Output = IntrusivePointer<C::Pointer, T>;

    fn load<K>(&self, key: &K) -> impl Future<Output = Self::Output> + Send
    where
        K: ?Sized + ToOwned<Owned = <T as crate::Value>::Key> + Hash + Eq,
        T::Key: Borrow<K>,
    {
        let this = &self.0;

        // XX: new one return type and don't want to wrap in async {} which requries K: Sync
        let futures = match this.cache.entry(&key) {
            Entry::Occupied(o) => {
                let pointer = o.into_pointer();
                Ok(async move {
                    match &pointer.0 {
                        ValueInner::Waiting { wakers, .. } => {
                            let wakers = Arc::clone(wakers);
                            WaitIntrusiveFut::new(self.clone(), pointer, wakers).await
                        }
                        ValueInner::Complete(_) => IntrusivePointer::new(pointer),
                    }
                })
            }
            Entry::Vacant(v) => {
                let wakers = Wakers::default();
                let pointer = v.insert(Value(ValueInner::Waiting {
                    key: key.to_owned(),
                    wakers: Arc::clone(&wakers),
                }));

                let key = pointer.key().clone();
                let load =
                    async move { this.insert_loaded_value(this.load.load::<T::Key>(&key).await) };
                let replace = WaitIntrusiveFut::new(self.clone(), pointer, wakers);

                Err(async move {
                    pin_mut!(load);
                    select(load, replace).await.factor_first().0
                })
            }
        };

        async move {
            match futures {
                Ok(f) => f.await,
                Err(f) => f.await,
            }
        }
    }
}

impl<T, L, C> Cache<T> for DedupLoadIntrusive<L, C>
where
    T: crate::Value + 'static,
    T::Key: Sized + Clone + Send,
    C: Cache<Value<T>> + Send + Sync + 'static,
    C::Pointer: Send + Sync,
{
    type Pointer = IntrusivePointer<C::Pointer, T>;

    fn len(&self) -> usize {
        self.0.cache.len()
    }

    fn iter(&self) -> impl Iterator<Item = Self::Pointer> {
        self.0
            .cache
            .iter()
            .filter(|p| matches!(p.0, ValueInner::Complete(_)))
            .map(IntrusivePointer::new)
    }

    fn entry<'c, 'k, K>(
        &'c self,
        key: &'k K,
    ) -> Entry<
        impl OccupiedEntry<Pointer = Self::Pointer> + 'c,
        impl VacantEntry<Pointer = Self::Pointer> + 'c,
    >
    where
        <T as crate::Value>::Key: Borrow<K>,
        K: ?Sized + Hash + Eq,
    {
        match self.0.cache.entry(key) {
            Entry::Occupied(o) => match &o.value().0 {
                ValueInner::Waiting { .. } => Entry::Vacant(Vacant(Some(VacantInner::Waiting(o)))),
                ValueInner::Complete(_) => Entry::Occupied(Occupied(o)),
            },
            Entry::Vacant(v) => Entry::Vacant(Vacant(Some(VacantInner::Vacant(v)))),
        }
    }
}

struct Occupied<O: OccupiedEntry>(O);

impl<T, O> OccupiedEntry for Occupied<O>
where
    T: crate::Value,
    T::Key: Sized,
    O: OccupiedEntry,
    O::Pointer: Deref<Target = Value<T>>,
{
    type Pointer = IntrusivePointer<O::Pointer, T>;

    fn value(&self) -> &<Self::Pointer as Deref>::Target {
        match &self.0.value().0 {
            ValueInner::Complete(v) => &v,
            _ => unreachable!(),
        }
    }

    fn pointer(&self) -> Self::Pointer {
        IntrusivePointer::new(self.0.pointer())
    }

    fn into_pointer(self) -> Self::Pointer {
        IntrusivePointer::new(self.0.into_pointer())
    }

    fn replace(self, value: T) -> Self::Pointer {
        IntrusivePointer::new(self.0.replace(Value(ValueInner::Complete(value))))
    }

    fn remove(self) -> Self::Pointer {
        IntrusivePointer::new(self.0.remove())
    }
}

struct Vacant<O: OccupiedEntry, V>(Option<VacantInner<O, V>>);

enum VacantInner<O, V> {
    Waiting(O),
    Vacant(V),
}

impl<T, O, V> VacantEntry for Vacant<O, V>
where
    T: crate::Value,
    T::Key: Sized,
    O: OccupiedEntry,
    O::Pointer: Deref<Target = Value<T>>,
    V: VacantEntry<Pointer = O::Pointer>,
{
    type Pointer = IntrusivePointer<O::Pointer, T>;

    fn insert(mut self, value: <Self::Pointer as Deref>::Target) -> Self::Pointer
    where
        <Self::Pointer as Deref>::Target: Sized,
    {
        match self.0.take().unwrap() {
            VacantInner::Waiting(occupied) => {
                let ValueInner::Waiting { wakers, .. } = &occupied.value().0 else {
                    unreachable!()
                };
                let wakers = Arc::clone(wakers);
                let pointer = occupied.replace(Value(ValueInner::Complete(value)));

                if let Some(mut wakers) = wakers.lock().take() {
                    wakers.drain().for_each(Waker::wake);
                }

                IntrusivePointer::new(pointer)
            }
            VacantInner::Vacant(v) => IntrusivePointer::new(v.insert(Value(ValueInner::Complete(value)))),
        }
    }
}

impl<L, C> DedupInner<L, C> {
    fn insert_loaded_value<T>(&self, value: T) -> IntrusivePointer<C::Pointer, T>
    where
        T: crate::Value,
        T::Key: Sized,
        C: Cache<Value<T>>,
    {
        match self.cache.entry::<T::Key>(value.key()) {
            Entry::Occupied(occupied) => match &occupied.value().0 {
                ValueInner::Waiting { wakers, .. } => {
                    let wakers = Arc::clone(wakers);
                    let pointer = IntrusivePointer::new(occupied.replace(Value(ValueInner::Complete(value))));
                    if let Some(mut wakers) = wakers.lock().take() {
                        wakers.drain().for_each(Waker::wake);
                    }
                    pointer
                }
                ValueInner::Complete(_) => {
                    IntrusivePointer::new(occupied.replace(Value(ValueInner::Complete(value))))
                }
            },
            Entry::Vacant(v) => IntrusivePointer::new(v.insert(Value(ValueInner::Complete(value)))),
        }
    }
}

struct WaitIntrusiveFut<T, L, C>
where
    T: crate::Value,
    T::Key: Sized,
    C: Cache<Value<T>>,
{
    dedup: DedupLoadIntrusive<L, C>,
    pointer: C::Pointer,
    wakers: Wakers,
    waker_key: Option<usize>,
    load_future: Option<Pin<Box<dyn Future<Output = IntrusivePointer<C::Pointer, T>> + Send>>>,
}

impl<T, L, C> Unpin for WaitIntrusiveFut<T, L, C>
where
    T: crate::Value,
    T::Key: Sized,
    C: Cache<Value<T>>,
{
}

impl<T, L, C> WaitIntrusiveFut<T, L, C>
where
    T: crate::Value,
    T::Key: Sized,
    C: Cache<Value<T>>,
{
    fn new(dedup: DedupLoadIntrusive<L, C>, pointer: C::Pointer, wakers: Wakers) -> Self {
        Self {
            dedup,
            pointer,
            wakers,
            waker_key: None,
            load_future: None,
        }
    }
}

impl<T, L, C> Future for WaitIntrusiveFut<T, L, C>
where
    T: crate::Value + 'static,
    T::Key: Sized + Clone + Send,
    L: AsyncLoad<T, Output = T> + Send + Sync + 'static,
    C: Cache<Value<T>> + Send + Sync + 'static,
    C::Pointer: Send + Sync,
{
    type Output = IntrusivePointer<C::Pointer, T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = Pin::into_inner(self);
        let mut found_no_wakers = false;

        loop {
            if let Some(load_future) = this.load_future.as_mut() {
                return load_future.as_mut().poll(cx);
            }

            if this.waker_key.is_none() {
                let waker = cx.waker().clone();
                if let Some(wakers) = this.wakers.lock().as_mut() {
                    this.waker_key = Some(wakers.insert(waker));
                    return Poll::Pending;
                }
            }

            match this.dedup.0.cache.entry(this.pointer.key()) {
                Entry::Occupied(occupied) => {
                    let pointer = occupied.into_pointer(); // drop occupied lock
                    match &pointer.0 {
                        ValueInner::Waiting { wakers, .. } => {
                            let Some(waker_key) = this.waker_key else {
                                this.wakers = Arc::clone(wakers);
                                continue;
                            };

                            if Arc::ptr_eq(wakers, &this.wakers) {
                                if let Some(wakers) = wakers.lock().as_mut() {
                                    wakers[waker_key].clone_from(cx.waker());
                                    return Poll::Pending;
                                }
                                // XX reload, we shouldn't land here twice in a row
                                debug_assert!(!found_no_wakers);
                                found_no_wakers = true;
                            } else {
                                this.waker_key = None;
                                this.wakers = Arc::clone(&wakers);
                            }
                        }
                        ValueInner::Complete(_) => {
                            this.waker_key = None;
                            return Poll::Ready(IntrusivePointer::new(pointer));
                        }
                    }
                }
                Entry::Vacant(v) => {
                    this.waker_key = None; // XX: do before key().clone() can panic
                    let pointer = v.insert(Value(ValueInner::Waiting {
                        key: this.pointer.key().clone(),
                        wakers: Default::default(),
                    }));

                    let dedup = this.dedup.clone();
                    // XX: actually need to just do the insert, not re-call load() because that would hang forever
                    // XX: unlikely situation, don't care about penalty of boxing
                    this.load_future =
                        Some(Box::pin(async move { dedup.load(pointer.key()).await }));
                }
            }
        }
    }
}

impl<T, L, C> Drop for WaitIntrusiveFut<T, L, C>
where
    T: crate::Value,
    T::Key: Sized,
    C: Cache<Value<T>>,
{
    fn drop(&mut self) {
        if let Some(waker_key) = self.waker_key.take() {
            if let Some(wakers) = self.wakers.lock().as_mut() {
                wakers.remove(waker_key);
            }
        }
    }
}

pub struct DedupLoad<L, LC, C>(Arc<DedupLoadInner<L, LC, C>>);

struct DedupLoadInner<L, LC, C> {
    load: L,
    load_cache: LC,
    cache: C,
}

impl<L, LC, C> Clone for DedupLoad<L, LC, C> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

struct Waiting<K> {
    key: K,
    wakers: Wakers,
}

impl<K: Eq + Hash> crate::Value for Waiting<K> {
    type Key = K;

    fn key(&self) -> &Self::Key {
        &self.key
    }
}

impl<T, L, LC, C> AsyncLoad<T> for DedupLoad<L, LC, C>
where
    T: crate::Value,
    T::Key: Sized + Clone + Send,
    L: AsyncLoad<T, Output = T> + Send + Sync,
    LC: Cache<Waiting<T::Key>> + Send + Sync,
    LC::Pointer: Send,
    C: Cache<T> + Send + Sync,
    C::Pointer: Send,
{
    type Output = C::Pointer;
    
    fn load<K>(&self, key: &K) -> impl Future<Output = Self::Output> + Send
    where
        K: ?Sized + ToOwned<Owned = <T as crate::Value>::Key> + Hash + Eq,
        <T as crate::Value>::Key: Borrow<K> 
    {
        let this = &self.0;
        let existing = this.cache.get(key).ok_or_else(|| key.to_owned());
        async move {
            match existing {
                Ok(pointer) => pointer,
                Err(key) => {
                    let lookup = match this.load_cache.entry::<T::Key>(&key) {
                        Entry::Occupied(occupied) => Ok(occupied.into_pointer()),
                        Entry::Vacant(vacant) => {
                            Err(vacant.insert(Waiting {
                                key,
                                wakers: Wakers::default(),
                            }))
                        },
                    };

                    // XX: avoid making entry() Send by separating
                    match lookup {
                        Ok(pointer) => {
                            WaitFut {
                                dedup: self.clone(),
                                wakers: Arc::clone(&pointer.wakers),
                                pointer,
                                waker_key: None,
                            }.await
                        },
                        Err(waiting) => {
                            let value = this.load.load::<T::Key>(waiting.key()).await;
                            let pointer = this.cache.insert(value);
                            waiting.wakers.lock().take().unwrap().drain().for_each(Waker::wake);
                            pointer
                        }
                    }
                }
            }
        }
    }
}

struct WaitFut<T, L, LC, C> 
where 
    T: crate::Value,
    T::Key: Sized,
    LC: Cache<Waiting<T::Key>>,
{
    dedup: DedupLoad<L, LC, C>,
    pointer: LC::Pointer,
    wakers: Wakers,
    waker_key: Option<usize>,
    // load_future: Option<Pin<Box<dyn Future<Output = IntrusivePointer<C::Pointer, T>> + Send>>>,
}

impl<T, L, LC, C> Unpin for WaitFut<T, L, LC, C>
where
    T: crate::Value,
    T::Key: Sized,
    LC: Cache<Waiting<T::Key>>,
{

}

impl<T, L, LC, C> Future for WaitFut<T, L, LC, C>
where
    T: crate::Value,
    T::Key: Sized,
    LC: Cache<Waiting<T::Key>>,
    C: Cache<T>,
{
    type Output = C::Pointer;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = Pin::into_inner(self);
        let mut found_no_wakers = false;

        loop {
            // if let Some(load_future) = this.load_future.as_mut() {
            //     return load_future.as_mut().poll(cx);
            // }

            if this.waker_key.is_none() {
                let waker = cx.waker().clone();
                if let Some(wakers) = this.wakers.lock().as_mut() {
                    this.waker_key = Some(wakers.insert(waker));
                    return Poll::Pending;
                }
            }

            match this.dedup.0.cache.entry(this.pointer.key()) {
                Entry::Occupied(occupied) => {
                    return Poll::Ready(occupied.into_pointer())
                }
                Entry::Vacant(v) => {
                    this.waker_key = None; // XX: do before key().clone() can panic
                    // let pointer = v.insert(Value(ValueInner::Waiting {
                    //     key: this.pointer.key().clone(),
                    //     wakers: Default::default(),
                    // }));

                    let dedup = this.dedup.clone();
                    // XX: unlikely situation, don't care about penalty of boxing
                    // this.load_future =
                    //     Some(Box::pin(async move { dedup.load(pointer.key()).await }));
                    todo!()
                }
            }
        }
    }
}