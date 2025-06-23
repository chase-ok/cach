use equivalent::{Comparable, Equivalent};
use stable_deref_trait::CloneStableDeref;
use std::array;
use std::hash::Hash;
use std::marker::PhantomData;
use std::ops::{Bound, Deref, RangeBounds};

pub mod map;
pub mod expire;
pub mod evict;
pub mod layer;

pub mod hash;

pub trait Pointer: Deref + CloneStableDeref {}

impl<P: Deref + CloneStableDeref> Pointer for P {}

pub trait Store<T> {
    type Pointer: Pointer<Target = T>;

    fn len(&self) -> usize;

    #[inline]
    fn is_empty(&self) -> bool {
        self.len() > 0
    }

    fn iter(&self) -> impl Iterator<Item = Self::Pointer>;

    #[inline]
    fn drain(&self) -> impl Iterator<Item = Self::Pointer> {
        self.extract_if(|_| true)
    }

    #[inline]
    fn retain(&self, mut f: impl FnMut(&Self::Pointer) -> bool) {
        self.extract_if(move |v| !f(v)).for_each(drop);
    }

    fn extract_if(
        &self,
        f: impl FnMut(&Self::Pointer) -> bool,
    ) -> impl Iterator<Item = Self::Pointer>;

    #[inline]
    fn clear(&self) {
        self.drain().for_each(drop);
    }

    fn insert(&self, value: T) -> Self::Pointer;

    fn or_insert(&self, value: T) -> Self::Pointer;

    fn remove(&self, value: &T) -> Option<Self::Pointer>;

    fn upsert(&self, value: T, f: impl for<'a> FnMut(&'a T, &'a Self::Pointer) -> &'a T) -> Self::Pointer;
}

// pub trait BuildStore {
//     type Store<T: 'static + hash::Value + Send + Sync, S>: Store<T>
//     where
//         S: BuildStrategy<T>;

//     fn build_store<T: 'static + hash::Value + Send + Sync>(self) -> Self::Store<T, NoStrategy>
//     where
//         Self: Sized
//     {
//         self.build_store_with_strategy(NoStrategy)
//     }

//     fn build_store_with_strategy<T: 'static + hash::Value + Send + Sync, S: BuildStrategy<T>>(self, layer: S) -> Self::Store<T, S>;
// }


#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Entry<O, V> {
    Occupied(O),
    Vacant(V)
}

pub trait HashOrdStore<T: HashOrdValue>: Store<T> {
    fn unique_keys(&self) -> impl Iterator<Item = impl Pointer<Target = T::Key>>;

    fn get<K, O>(&self, key: &K, ord: &O) -> Option<Self::Pointer>
    where
        K: ?Sized + Hash + Equivalent<T::Key>,
        O: ?Sized + Comparable<T::Ord>;

    #[inline]
    fn get_many<K, O, const N: usize>(&self, keys: [(&K, &O); N]) -> [Option<Self::Pointer>; N]
    where
        K: ?Sized + Hash + Equivalent<T::Key>,
        O: ?Sized + Comparable<T::Ord>,
    {
        array::from_fn(|i| self.get(keys[i].0, keys[i].1))
    }

    fn next<K, O>(&self, key: &K, ord: Bound<&O>) -> Option<Self::Pointer>
    where
        K: ?Sized + Hash + Equivalent<T::Key>,
        O: ?Sized + Comparable<T::Ord>;

    fn next_back<K, O>(&self, key: &K, ord: Bound<&O>) -> Option<Self::Pointer>
    where
        K: ?Sized + Hash + Equivalent<T::Key>,
        O: ?Sized + Comparable<T::Ord>;

    #[inline]
    fn first<K>(&self, key: &K) -> Option<Self::Pointer>
    where
        K: ?Sized + Hash + Equivalent<T::Key>,
    {
        self.next(key, Bound::Unbounded::<&T::Ord>)
    }

    #[inline]
    fn last<K>(&self, key: &K) -> Option<Self::Pointer>
    where
        K: ?Sized + Hash + Equivalent<T::Key>,
    {
        self.next_back(key, Bound::Unbounded::<&T::Ord>)
    }

    // XX not efficient!
    #[inline]
    fn range<'a, K, O>(
        &'a self,
        key: &'a K,
        range: impl RangeBounds<O> + 'a,
    ) -> impl DoubleEndedIterator<Item = Self::Pointer> + 'a
    where
        K: ?Sized + Hash + Equivalent<T::Key>,
        O: ?Sized + Comparable<T::Ord> + 'a,
        T: 'a,
    {
        struct Iter<'a, S, T, K, R, O>
        where
            S: ?Sized + HashOrdStore<T>,
            T: HashOrdValue,
            K: ?Sized,
            O: ?Sized,
        {
            store: &'a S,
            key: &'a K,
            start: Option<S::Pointer>,
            end: Option<S::Pointer>,
            range: R,
            _marker: PhantomData<(T, O)>,
        }

        impl<'a, S, T, K, R, O> Iterator for Iter<'a, S, T, K, R, O>
        where
            S: ?Sized + HashOrdStore<T>,
            T: HashOrdValue + 'a,
            K: ?Sized + Hash + Equivalent<T::Key>,
            R: RangeBounds<O>,
            O: ?Sized + Comparable<T::Ord>,
        {
            type Item = S::Pointer;

            fn next(&mut self) -> Option<Self::Item> {
                let next = match self.start.take() {
                    Some(start) => self.store.next(self.key, Bound::Excluded(start.ord())),
                    None => self.store.next(self.key, self.range.start_bound()),
                };

                let value = next.filter(|value| {
                    if let Some(end) = &self.end {
                        value.ord() < end.ord()
                    } else {
                        match self.range.end_bound() {
                            Bound::Included(i) => i.compare(value.ord()).is_ge(),
                            Bound::Excluded(e) => e.compare(value.ord()).is_gt(),
                            Bound::Unbounded => true,
                        }
                    }
                })?;

                self.start = Some(value.clone());
                Some(value)
            }
        }

        impl<'a, S, T, K, R, O> DoubleEndedIterator for Iter<'a, S, T, K, R, O>
        where
            S: ?Sized + HashOrdStore<T>,
            T: HashOrdValue + 'a,
            K: ?Sized + Hash + Equivalent<T::Key>,
            R: RangeBounds<O>,
            O: ?Sized + Comparable<T::Ord>,
        {
            fn next_back(&mut self) -> Option<Self::Item> {
                let next_back = match self.end.take() {
                    Some(end) => self.store.next_back(self.key, Bound::Excluded(end.ord())),
                    None => self.store.next_back(self.key, self.range.end_bound()),
                };

                let value = next_back.filter(|value| {
                    if let Some(start) = &self.start {
                        start.ord() < value.ord()
                    } else {
                        match self.range.start_bound() {
                            Bound::Included(i) => i.compare(value.ord()).is_le(),
                            Bound::Excluded(e) => e.compare(value.ord()).is_lt(),
                            Bound::Unbounded => true,
                        }
                    }
                })?;

                self.start = Some(value.clone());
                Some(value)
            }
        }

        Iter {
            store: self,
            key,
            start: None,
            end: None,
            range,
            _marker: PhantomData,
        }
    }

    fn or_insert_with<K, O>(&self, key: K, ord: O, value: impl FnOnce(K, O) -> T) -> Self::Pointer
    where
        K: Hash + Equivalent<T::Key>,
        O: Comparable<T::Ord>;

    fn remove_key_ord<K, O>(&self, key: &K, ord: &O) -> Option<Self::Pointer>
    where
        K: ?Sized + Hash + Equivalent<T::Key>,
        O: ?Sized + Comparable<T::Ord>;
}

pub trait HashOrdValue: hash::Value + OrdValue {}

impl<V: hash::Value + OrdValue + ?Sized> HashOrdValue for V {}

pub trait OrdStore<T: OrdValue>: Store<T> {
    fn get<O>(&self, ord: &O) -> Option<Self::Pointer>
    where
        O: ?Sized + Comparable<T::Ord>;

    #[inline]
    fn get_many<O, const N: usize>(&self, ords: [&O; N]) -> [Option<Self::Pointer>; N]
    where
        O: ?Sized + Comparable<T::Ord>,
    {
        array::from_fn(|i| self.get(ords[i]))
    }

    fn next<O>(&self, ord: Bound<&O>) -> Option<Self::Pointer>
    where
        O: ?Sized + Comparable<T::Ord>;

    fn next_back<O>(&self, ord: Bound<&O>) -> Option<Self::Pointer>
    where
        O: ?Sized + Comparable<T::Ord>;

    #[inline]
    fn first(&self) -> Option<Self::Pointer> {
        self.next(Bound::Unbounded::<&T::Ord>)
    }

    #[inline]
    fn last(&self) -> Option<Self::Pointer> {
        self.next_back(Bound::Unbounded::<&T::Ord>)
    }

    // XX not efficient!
    #[inline]
    fn range<'a, O>(
        &'a self,
        range: impl RangeBounds<O> + 'a,
    ) -> impl DoubleEndedIterator<Item = Self::Pointer> + 'a
    where
        O: ?Sized + Comparable<T::Ord> + 'a,
        T: 'a,
    {
        struct Iter<'a, S, T, R, O>
        where
            S: ?Sized + OrdStore<T>,
            T: OrdValue,
            O: ?Sized,
        {
            store: &'a S,
            start: Option<S::Pointer>,
            end: Option<S::Pointer>,
            range: R,
            _marker: PhantomData<(T, O)>,
        }

        impl<'a, S, T, R, O> Iterator for Iter<'a, S, T, R, O>
        where
            S: ?Sized + OrdStore<T>,
            T: OrdValue + 'a,
            R: RangeBounds<O>,
            O: ?Sized + Comparable<T::Ord>,
        {
            type Item = S::Pointer;

            fn next(&mut self) -> Option<Self::Item> {
                let next = match self.start.take() {
                    Some(start) => self.store.next(Bound::Excluded(start.ord())),
                    None => self.store.next(self.range.start_bound()),
                };

                let value = next.filter(|value| {
                    if let Some(end) = &self.end {
                        value.ord() < end.ord()
                    } else {
                        match self.range.end_bound() {
                            Bound::Included(i) => i.compare(value.ord()).is_ge(),
                            Bound::Excluded(e) => e.compare(value.ord()).is_gt(),
                            Bound::Unbounded => true,
                        }
                    }
                })?;

                self.start = Some(value.clone());
                Some(value)
            }
        }

        impl<'a, S, T, R, O> DoubleEndedIterator for Iter<'a, S, T, R, O>
        where
            S: ?Sized + OrdStore<T>,
            T: OrdValue + 'a,
            R: RangeBounds<O>,
            O: ?Sized + Comparable<T::Ord>,
        {
            fn next_back(&mut self) -> Option<Self::Item> {
                let next_back = match self.end.take() {
                    Some(end) => self.store.next_back(Bound::Excluded(end.ord())),
                    None => self.store.next_back(self.range.end_bound()),
                };

                let value = next_back.filter(|value| {
                    if let Some(start) = &self.start {
                        start.ord() < value.ord()
                    } else {
                        match self.range.start_bound() {
                            Bound::Included(i) => i.compare(value.ord()).is_le(),
                            Bound::Excluded(e) => e.compare(value.ord()).is_lt(),
                            Bound::Unbounded => true,
                        }
                    }
                })?;

                self.start = Some(value.clone());
                Some(value)
            }
        }

        Iter {
            store: self,
            start: None,
            end: None,
            range,
            _marker: PhantomData,
        }
    }

    fn or_insert_with<O>(&self, ord: O, value: impl FnOnce(O) -> T) -> Self::Pointer
    where
        O: Comparable<T::Ord>;

}

pub trait OrdValue {
    type Ord: ?Sized + Ord;

    fn ord(&self) -> &Self::Ord;
}

#[cfg(test)]
mod tests {
    // use std::time::{Duration, Instant};

    // use super::{expire::{ExpireAfterWrite, ExpireAt, ExpireAtIntrusive}, BuildStore, hash::Value, Store};

    // struct TestValue {
    //     x: u64,
    //     expire_at: Instant,
    // }

    // impl Value for TestValue {
    //     type Key = u64;

    //     fn key(&self) -> &Self::Key {
    //         &self.x
    //     }
    // }

    // impl ExpireAt for TestValue {
    //     fn expire_at(&self) -> Instant {
    //         self.expire_at
    //     }
    // }

    // fn compiles(x: impl BuildStore) {
    //     let store = x.build_store_with_strategy::<TestValue, _>(ExpireAtIntrusive::new());
    //     let p = store.insert(TestValue { x: 1, expire_at: Instant::now() });
    //     p.x;
    // }
}