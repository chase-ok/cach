use std::{borrow::Borrow, hash::{BuildHasher, Hash, Hasher, RandomState}, ops::Deref, sync::Arc};

use scc::{ebr::Guard, Equivalent};

use crate::{atomic::Mutate, Value};

use super::{Compute, Mutate};

pub struct Cache<T, S: BuildHasher = RandomState>(scc::HashIndex<Item<T>, (), S>);

#[repr(transparent)]
pub struct Pointer<T>(Arc<T>);

struct Item<T>(Arc<T>);

impl<T> Clone for Pointer<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Deref for Pointer<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> Clone for Item<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: Value> PartialEq for Item<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0.key() == other.0.key()
    }
}

impl<T: Value> Eq for Item<T> { }

impl<T: Value> Hash for Item<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.key().hash(state);
    }
}

#[derive(Hash)]
struct Key<'a, K: ?Sized>(&'a K);

impl<'a, T: Value, K> Equivalent<Item<T>> for Key<'a, K>
where
    T::Key: Borrow<K>,
    K: ?Sized + Eq
{
    fn equivalent(&self, item: &Item<T>) -> bool {
        self.0.equivalent(item.0.key())
    }
}


impl<T: Value + 'static, S: BuildHasher> super::Cache<T> for Cache<T, S> {
    type Pointer = Pointer<T>;

    fn len(&self) -> usize {
        self.0.len()
    }

    fn iter(&self) -> impl Iterator<Item = Self::Pointer> {
        [].into_iter() // XX
    }

    fn compute<R>(
        &self,
        value: T,
        mut f: impl FnMut(Option<T>, Option<&Self::Pointer>) -> Mutate<T, R>,
    ) -> Compute<Self::Pointer, R> {
        let item = Item(Arc::new(value));
        match self.0.entry(item.clone()) {
            scc::hash_index::Entry::Occupied(occ) => {
                let value = Arc::into_inner(item.0).unwrap();
                match f(Some(value), occ.key()) {
                    Mutate::None(r) => Compute::None(r),
                    Mutate::Insert(v) => occ.remove_entry(),
                    Mutate::Remove => todo!(),
                }
            }
            scc::hash_index::Entry::Vacant(vac) => todo!(),
        }
        todo!()
    }

    fn compute_key<K, R>(
        &self,
        key: &K,
        f: impl FnMut(Option<T>, Option<&Self::Pointer>) -> Mutate<T, R>,
    ) -> Compute<Self::Pointer, R>
    where
        <T as Value>::Key: Borrow<K>,
        K: ?Sized + Hash + Eq {
        todo!()
    }
}