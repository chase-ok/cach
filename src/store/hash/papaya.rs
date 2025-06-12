use std::{
    hash::{BuildHasher, Hash},
    ops::Deref,
    sync::Arc,
};

use equivalent::Equivalent;
use hashbrown::HashTable;
use papaya::{Compute, LocalGuard, Operation};
use ref_cast::RefCast;
use stable_deref_trait::{CloneStableDeref, StableDeref};

use crate::store::{
    Entry, Store as _,
    layer::{Layer, LayerPointer, Operate, Purge, Remove, StartRead},
};

use super::Store as _;

#[derive(RefCast)]
#[repr(transparent)]
pub struct Pointer<T, V>(Arc<Value<T, V>>);

impl<T, V> Pointer<T, V> {
    fn from_ref(arc: &Arc<Value<T, V>>) -> Self {
        Self(Arc::clone(arc))
    }
}

impl<T, Sv> Clone for Pointer<T, Sv> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T, Sv> Deref for Pointer<T, Sv> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0.user
    }
}

// XX
unsafe impl<T, Sv> StableDeref for Pointer<T, Sv> {}
unsafe impl<T, Sv> CloneStableDeref for Pointer<T, Sv> {}

impl<T, L> LayerPointer for Pointer<T, L> {
    type LayerTarget = L;

    fn layer(&self) -> &Self::LayerTarget {
        &self.0.layer
    }
}

#[doc(hidden)]
pub struct Value<T, L> {
    user: T,
    layer: L,
}

impl<T: super::Value, L> Hash for Value<T, L> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.user.key().hash(state);
    }
}

impl<T: super::Value, L> PartialEq for Value<T, L> {
    fn eq(&self, other: &Self) -> bool {
        self.user.key() == other.user.key()
    }
}

impl<T: super::Value, L> Eq for Value<T, L> {}

struct KeyRef<'a, K: ?Sized>(&'a K);

impl<K, T, L> Equivalent<Arc<Value<T, L>>> for KeyRef<'_, K>
where
    K: ?Sized + Equivalent<T::Key>,
    T: super::Value,
{
    fn equivalent(&self, value: &Arc<Value<T, L>>) -> bool {
        self.0.equivalent(value.user.key())
    }
}

impl<K: ?Sized + Hash> Hash for KeyRef<'_, K> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl<K: ?Sized + PartialEq> PartialEq for KeyRef<'_, K> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl<K: ?Sized + Eq> Eq for KeyRef<'_, K> {}

pub struct Store<T, L, V, H: BuildHasher = std::hash::RandomState> {
    map: papaya::HashMap<Arc<Value<T, V>>, (), H>,
    layer: L,
    hasher: H,
}

impl<T, L, V, H> crate::store::Store<T> for Store<T, L, V, H>
where
    T: super::Value + Send + Sync + 'static,
    L: Layer<Pointer<T, V>, Value = V>,
    V: Send + Sync + 'static,
    H: BuildHasher,
{
    type Pointer = Pointer<T, V>;

    fn len(&self) -> usize {
        self.map.len()
    }

    fn iter(&self) -> impl Iterator<Item = Self::Pointer> {
        // XX: borrow preventing lazy iterator :(
        let mut vec = Vec::with_capacity(self.len());
        vec.extend(self.map.pin().keys().map(Pointer::from_ref));
        vec.into_iter()
    }

    fn extract_if(
        &self,
        mut f: impl FnMut(&Self::Pointer) -> bool,
    ) -> impl Iterator<Item = Self::Pointer> {
        // XX: borrow preventing lazy iterator :(
        let mut set = HashTable::<Pointer<T, V>>::with_capacity(self.len());
        let hash = |p: &Pointer<T, V>| self.hasher.hash_one(Arc::as_ptr(&p.0).addr());
        self.map.pin().retain(|arc, ()| {
            let pointer = Pointer::ref_cast(arc);
            let retain = !f(pointer);
            if !retain {
                set.entry(hash(pointer), |p| Arc::ptr_eq(&p.0, arc), hash)
                    .or_insert_with(|| pointer.clone());
            }
            retain
        });
        set.into_iter()
    }

    fn clear(&self) {
        self.map.pin().clear();
    }

    fn insert(&self, value: T) -> Self::Pointer {
        let guard = self.map.guard();
        self.do_insert(value, &guard)
    }

    #[inline]
    fn or_insert(&self, value: T) -> Self::Pointer {
        self.upsert(value, |_new, existing| &existing)
    }

    #[inline]
    fn remove(&self, value: &T) -> Option<Self::Pointer> {
        self.remove_key_if(value.key(), |existing| std::ptr::eq(value, &**existing))
            .unwrap_or_default()
    }

    fn upsert(
        &self,
        value: T,
        mut f: impl for<'a> FnMut(&'a T, &'a Self::Pointer) -> &'a T,
    ) -> Self::Pointer {
        let guard = self.map.guard();
        let mut operate = self.operate(&guard);

        let arc = Arc::new(Value {
            layer: operate.start_insert(&value),
            user: value,
        });
        let computed = self.map.compute(
            arc.clone(),
            |existing| match existing {
                Some((existing, ())) => {
                    let existing = Pointer::ref_cast(existing);
                    match operate.start_read(existing) {
                        StartRead::Allow => {
                            let chosen = f(&arc.user, existing);
                            if std::ptr::eq(chosen, &**existing) {
                                Operation::Abort(existing)
                            } else {
                                assert!(std::ptr::eq(chosen, &arc.user));
                                Operation::Insert(())
                            }
                        }
                        StartRead::Remove => Operation::Insert(()),
                    }
                }
                None => Operation::Insert(()),
            },
            &guard,
        );

        match computed {
            Compute::Inserted(arc, ()) => {
                let pointer = Pointer::from_ref(arc);
                operate.complete_insert(&pointer);
                pointer
            }
            Compute::Updated { old, new } => {
                // XX not bothering to call operate.read()
                operate.remove(Pointer::ref_cast(old.0));
                let pointer = Pointer::from_ref(new.0);
                operate.complete_insert(&pointer);
                pointer
            }
            Compute::Aborted(pointer) => {
                operate.complete_read(pointer);
                pointer.clone()
            }
            _ => unreachable!(),
        }
    }
}

impl<T, L, V, H> super::Store<T> for Store<T, L, V, H>
where
    T: super::Value + Send + Sync + 'static,
    L: Layer<Pointer<T, V>, Value = V>,
    V: Send + Sync + 'static,
    H: BuildHasher,
{
    #[allow(refining_impl_trait)]
    fn entry<'a, K>(
        &'a self,
        key: &K,
    ) -> Entry<OccupiedEntry<'a, T, L, V, H>, VacantEntry<'a, T, L, V, H>>
    where
        K: ?Sized + Hash + Equivalent<<T as super::Value>::Key>,
    {
        todo!()
    }

    fn get(&self, key: &(impl ?Sized + Hash + Equivalent<T::Key>)) -> Option<Self::Pointer> {
        let guard = self.map.guard();
        let mut operate = self.operate(&guard);
        match self.map.get_key_value(&KeyRef(key), &guard) {
            Some((arc, ())) => {
                let pointer = Pointer::ref_cast(arc);
                match operate.start_read(pointer) {
                    StartRead::Allow => {
                        operate.complete_read(pointer);
                        Some(pointer.clone())
                    }
                    StartRead::Remove => {
                        let computed = self.map.compute(
                            Arc::clone(&pointer.0),
                            |existing| match existing {
                                Some((arc, ())) if Arc::ptr_eq(arc, &pointer.0) => {
                                    Operation::Remove
                                }
                                Some((arc, ())) => {
                                    let pointer = Pointer::ref_cast(arc);
                                    match operate.start_read(pointer) {
                                        StartRead::Allow => Operation::Abort(Some(pointer)),
                                        StartRead::Remove => Operation::Remove,
                                    }
                                }
                                None => Operation::Abort(None),
                            },
                            &guard,
                        );
                        match computed {
                            Compute::Removed(arc, ()) => {
                                let _ = operate.remove(Pointer::ref_cast(arc));
                                None
                            }
                            Compute::Aborted(pointer) => {
                                pointer.inspect(|p| operate.complete_read(p)).cloned()
                            }
                            _ => unreachable!(),
                        }
                    }
                }
            }
            None => None,
        }
    }

    fn or_insert_with<K>(&self, key: K, value: impl FnOnce(K) -> T) -> Self::Pointer
    where
        K: Hash + Equivalent<T::Key>,
    {
        let guard = self.map.guard();
        let mut operate = self.operate(&guard);
        match self.map.get_key_value(&KeyRef(&key), &guard) {
            Some((arc, ())) => {
                let pointer = Pointer::ref_cast(arc);
                match operate.start_read(pointer) {
                    StartRead::Allow => {
                        operate.complete_read(pointer);
                        pointer.clone()
                    }
                    StartRead::Remove => {
                        let value = value(key);
                        let computed = self.map.compute(
                            Arc::new(Value {
                                layer: operate.start_insert(&value),
                                user: value,
                            }),
                            |existing| match existing {
                                Some((arc, ())) if Arc::ptr_eq(arc, &pointer.0) => {
                                    Operation::Insert(())
                                }
                                Some((arc, ())) => Operation::Abort(arc),
                                None => Operation::Insert(()),
                            },
                            &guard,
                        );
                        match computed {
                            Compute::Inserted(arc, ()) => {
                                let pointer = Pointer::from_ref(arc);
                                operate.complete_insert(&pointer);
                                pointer
                            }
                            Compute::Updated { old, new } => {
                                operate.remove(Pointer::ref_cast(old.0));
                                let pointer = Pointer::from_ref(new.0);
                                operate.complete_insert(&pointer);
                                pointer
                            }
                            Compute::Aborted(arc) => Pointer::from_ref(arc),
                            _ => unreachable!(),
                        }
                    }
                }
            }
            None => self.or_insert(value(key)),
        }
    }

    fn remove_key_if(
        &self,
        key: &(impl ?Sized + Hash + Equivalent<T::Key>),
        f: impl FnMut(&Self::Pointer) -> bool,
    ) -> Result<Option<Self::Pointer>, Self::Pointer> {
        let guard = self.map.guard();
        self.do_remove_key_if(key, f, &guard)
    }
}

impl<T, L, V, H> Store<T, L, V, H>
where
    T: super::Value + Send + Sync + 'static,
    L: Layer<Pointer<T, V>, Value = V>,
    V: Send + Sync + 'static,
    H: BuildHasher,
{
    fn operate<'a>(
        &'a self,
        guard: &LocalGuard<'_>,
    ) -> impl Operate<Pointer<T, V>> + use<'a, T, L, V, H> {
        struct Guarded<'a, T, L, V, H: BuildHasher> {
            guard: &'a LocalGuard<'a>,
            this: &'a Store<T, L, V, H>,
        }

        impl<T, L, V, H> Purge<'_, Pointer<T, V>> for Guarded<'_, T, L, V, H>
        where
            T: super::Value + Send + Sync + 'static,
            L: Layer<Pointer<T, V>, Value = V>,
            V: Send + Sync + 'static,
            H: BuildHasher,
        {
            fn try_remove(&mut self, pointer: &Pointer<T, V>) -> Result<(), ()> {
                match self.this.map.remove_if(
                    &pointer.0,
                    |a, ()| Arc::ptr_eq(&pointer.0, a),
                    self.guard,
                ) {
                    Ok(Some(_)) => Ok(()),
                    _ => Err(()),
                }
            }
        }

        let mut operate = self.layer.operate();
        operate.purge(Guarded { guard, this: self });
        operate
    }

    fn do_insert(&self, value: T, guard: &LocalGuard<'_>) -> Pointer<T, V> {
        let mut operate = self.operate(guard);

        let computed = self.map.compute(
            Arc::new(Value {
                layer: operate.start_insert(&value),
                user: value,
            }),
            |_| Operation::Insert::<_, ()>(()),
            guard,
        );

        match computed {
            Compute::Inserted(arc, ()) => {
                let pointer = Pointer::from_ref(arc);
                operate.complete_insert(&pointer);
                pointer
            }
            Compute::Updated { old, new } => {
                let pointer = Pointer::from_ref(new.0);
                operate.complete_insert(&pointer);
                operate.remove(&Pointer::ref_cast(old.0));
                pointer
            }
            _ => unreachable!(),
        }
    }

    fn do_try_insert<'a>(
        &'a self,
        value: T,
        expected: Option<&Pointer<T, V>>,
        guard: LocalGuard<'a>,
    ) -> Result<
        Pointer<T, V>,
        (
            T,
            Entry<OccupiedEntry<'a, T, L, V, H>, VacantEntry<'a, T, L, V, H>>,
        ),
    > {
        let mut operate = self.operate(&guard);
        let new_arc = Arc::new(Value {
            layer: operate.start_insert(&value),
            user: value,
        });

        let computed = self.map.compute(
            new_arc.clone(),
            |existing| match existing {
                Some((arc, ())) => {
                    let pointer = Pointer::ref_cast(arc);
                    if expected.is_some_and(|e| Arc::ptr_eq(&e.0, arc)) {
                        // already checked read before this
                        Operation::Insert(())
                    } else {
                        match operate.start_read(pointer) {
                            StartRead::Allow => Operation::Abort(Some(pointer)),
                            StartRead::Remove => {
                                if expected.is_none() {
                                    Operation::Insert(())
                                } else {
                                    Operation::Remove
                                }
                            }
                        }
                    }
                }
                None => {
                    if expected.is_none() {
                        Operation::Insert(())
                    } else {
                        Operation::Abort(None)
                    }
                }
            },
            &guard,
        );

        match computed {
            Compute::Inserted(arc, ()) => {
                let pointer = Pointer::from_ref(arc);
                operate.complete_insert(&pointer);
                Ok(pointer)
            }
            Compute::Updated { old, new } => {
                let pointer = Pointer::from_ref(new.0);
                operate.complete_insert(&pointer);
                operate.remove(&Pointer::ref_cast(old.0));
                Ok(pointer)
            }
            Compute::Removed(arc, ()) => {
                operate.remove(Pointer::ref_cast(arc));
                let value = Arc::into_inner(new_arc).unwrap();
                Err((
                    value.user,
                    Entry::Vacant(VacantEntry { guard, store: self }),
                ))
            }
            Compute::Aborted(pointer) => {
                let value = Arc::into_inner(new_arc).unwrap();
                operate.fail_insert(&value.user, value.layer);
                Err((
                    value.user,
                    match pointer {
                        Some(pointer) => {
                            operate.complete_read(pointer);
                            Entry::Occupied(OccupiedEntry {
                                store: self,
                                pointer: pointer.clone(),
                                guard,
                            })
                        }
                        None => Entry::Vacant(VacantEntry { guard, store: self }),
                    },
                ))
            }
        }
    }

    fn do_remove_key_if(
        &self,
        key: &(impl ?Sized + Hash + Equivalent<T::Key>),
        mut f: impl FnMut(&Pointer<T, V>) -> bool,
        guard: &LocalGuard<'_>,
    ) -> Result<Option<Pointer<T, V>>, Pointer<T, V>> {
        let mut operate = self.operate(&guard);
        let mut read_allowed = false;
        let removed = self.map.remove_if(
            &KeyRef(key),
            |arc, ()| {
                let pointer = Pointer::ref_cast(arc);
                match operate.start_read(pointer) {
                    StartRead::Allow => {
                        read_allowed = true;
                        f(pointer)
                    }
                    StartRead::Remove => {
                        read_allowed = false;
                        true
                    }
                }
            },
            guard,
        );
        match (removed, read_allowed) {
            (Ok(Some((arc, ()))), _) => {
                let pointer = Pointer::from_ref(arc);
                operate.remove(&pointer);
                Ok(Some(pointer))
            }
            (Ok(None), _) => Ok(None),
            (Err((arc, ())), true) => {
                let pointer = Pointer::from_ref(arc);
                operate.complete_read(&pointer);
                Err(pointer)
            }
            (Err((arc, ())), false) => {
                operate.remove(Pointer::ref_cast(arc));
                Ok(None)
            }
        }
    }
}

pub struct OccupiedEntry<'a, T, L, V, H: BuildHasher> {
    guard: LocalGuard<'a>,
    store: &'a Store<T, L, V, H>,
    pointer: Pointer<T, V>,
}

impl<'a, T, L, V, H> super::OccupiedEntry<'a> for OccupiedEntry<'a, T, L, V, H>
where
    T: super::Value + Send + Sync + 'static,
    L: Layer<Pointer<T, V>, Value = V>,
    V: Send + Sync + 'static,
    H: BuildHasher,
{
    type Value = T;
    type Pointer = Pointer<T, V>;
    type VacantEntry = VacantEntry<'a, T, L, V, H>;

    fn get(&self) -> &Self::Value {
        &self.pointer
    }

    fn pointer(&self) -> &Self::Pointer {
        &self.pointer
    }

    fn into_pointer(self) -> Self::Pointer {
        self.pointer
    }

    fn remove(self) -> Result<Self::Pointer, Entry<Self, Self::VacantEntry>> {
        match self.store.do_remove_key_if(
            self.pointer.key(),
            |existing| Arc::ptr_eq(&self.pointer.0, &existing.0),
            &self.guard,
        ) {
            Ok(Some(pointer)) => Ok(pointer),
            Ok(None) => Err(Entry::Vacant(VacantEntry { guard: self.guard, store: self.store })),
            Err(pointer) => Err(Entry::Occupied(OccupiedEntry { guard: self.guard, store: self.store, pointer }))
        }
    }

    fn remove_key(self) -> Option<Self::Pointer> {
        self.store
            .do_remove_key_if(self.pointer.key(), |_| true, &self.guard)
            .unwrap_or_default()
    }

    fn try_insert(
        self,
        value: Self::Value,
    ) -> Result<Self::Pointer, (Self::Value, Entry<Self, Self::VacantEntry>)> {
        assert!(
            self.pointer.key() == value.key(),
            "tried to insert a value with a different key"
        );
        self.store
            .do_try_insert(value, Some(&self.pointer), self.guard)
    }

    fn insert(self, value: Self::Value) -> Self::Pointer {
        assert!(
            self.pointer.key() == value.key(),
            "tried to insert a value with a different key"
        );
        self.store.do_insert(value, &self.guard)
    }
}

pub struct VacantEntry<'a, T, L, V, H: BuildHasher> {
    guard: LocalGuard<'a>,
    store: &'a Store<T, L, V, H>,
}

impl<'a, T, L, V, H> super::VacantEntry<'a> for VacantEntry<'a, T, L, V, H>
where
    T: super::Value + Send + Sync + 'static,
    L: Layer<Pointer<T, V>, Value = V>,
    V: Send + Sync + 'static,
    H: BuildHasher,
{
    type Value = T;
    type Pointer = Pointer<T, V>;
    type OccupiedEntry = OccupiedEntry<'a, T, L, V, H>;

    fn try_insert(
        self,
        value: Self::Value,
    ) -> Result<Self::Pointer, (Self::Value, Self::OccupiedEntry)> {
        self.store
            .do_try_insert(value, None, self.guard)
            .map_err(|(value, entry)| match entry {
                Entry::Occupied(o) => (value, o),
                _ => unreachable!(),
            })
    }

    fn insert(self, value: Self::Value) -> Self::Pointer {
        self.store.do_insert(value, &self.guard)
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn replaces() {
        #[derive(Debug)]
        struct Key {
            value: u8,
            id: u8,
        }

        impl std::hash::Hash for Key {
            fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                self.value.hash(state);
            }
        }

        impl PartialEq for Key {
            fn eq(&self, other: &Self) -> bool {
                self.value == other.value
            }
        }

        impl Eq for Key {}

        let map = papaya::HashMap::<Key, ()>::new();
        let guard = map.guard();
        map.insert(Key { value: 0, id: 0 }, (), &guard);
        map.insert(Key { value: 0, id: 1 }, (), &guard);
        dbg!(map.get_key_value(&Key { value: 0, id: 2 }, &guard));
    }
}
