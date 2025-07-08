use std::{
    hash::{BuildHasher, Hash},
    mem,
    ops::{AddAssign, Deref},
    sync::Arc,
};

use crossbeam_utils::CachePadded;
use equivalent::Equivalent;
use hashbrown::DefaultHashBuilder;
use parking_lot::{
    RawRwLock, RwLock, RwLockReadGuard, RwLockUpgradableReadGuard, RwLockWriteGuard,
};
use smallvec::SmallVec;
use stable_deref_trait::{CloneStableDeref, StableDeref};

use crate::{
    store::{
        Entry,
        hash::Store,
        layer::{Layer, LayerPointer, Operate, Purge, Remove, StartRead},
    },
    thread::ThreadShardedCounter,
};

pub struct Pointer<T, V>(Arc<Value<T, V>>);

impl<T, V> Pointer<T, V> {
    fn new(user: T, operate: &mut impl Operate<Self>) -> Self {
        Self(Arc::new(Value {
            layer: operate.start_insert(&user),
            user,
        }))
    }
}

#[doc(hidden)]
pub struct Value<T, V> {
    user: T,
    layer: V,
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

pub struct FixedCapSyncHashStore<T, L, V, H = hashbrown::DefaultHashBuilder> {
    table: Box<[Lockbox<T, V>]>,
    mask: usize,
    cap: usize,
    len: ThreadShardedCounter,
    layer: L,
    hasher: H,
}

impl<T, L, V, H> FixedCapSyncHashStore<T, L, V, H> {
    pub fn with_capacity(cap: usize) -> Self
    where
        L: Default,
        H: Default,
    {
        Self::with_capacity_and_layer(cap, Default::default())
    }

    pub fn with_capacity_and_layer(cap: usize, layer: L) -> Self
    where
        H: Default,
    {
        let slots = (cap as f64 / 0.75).ceil() as usize; // XX load factor
        let num_lockboxes = slots
            .div_ceil(LOCKBOX_LEN)
            // XX don't need any more, along with mask?
            .checked_next_power_of_two()
            .expect("out of capacity");
        Self {
            table: std::iter::repeat_with(empty_lockbox)
                .take(num_lockboxes)
                .collect(),
            mask: num_lockboxes - 1,
            cap,
            len: Default::default(),
            layer,
            hasher: Default::default(),
        }
    }
}

impl<T, L, V, H> crate::store::Store<T> for FixedCapSyncHashStore<T, L, V, H>
where
    T: super::Value + Send + Sync + 'static,
    L: Layer<Pointer<T, V>, Value = V>,
    V: Send + Sync + 'static,
    H: BuildHasher,
{
    type Pointer = Pointer<T, V>;

    fn len(&self) -> usize {
        usize::try_from(self.len.get()).unwrap_or(0)
    }

    fn iter(&self) -> impl Iterator<Item = Self::Pointer> {
        self.table.iter().flat_map(|lockbox| {
            lockbox
                .read()
                .iter()
                .flatten()
                .cloned()
                .collect::<SmallVec<[_; LOCKBOX_LEN]>>()
        })
    }

    fn extract_if<'a>(
        &'a self,
        mut f: impl FnMut(&Self::Pointer) -> bool + 'a,
    ) -> impl Iterator<Item = Self::Pointer> + 'a {
        self.table.iter().flat_map(move |lockbox| {
            let mut extracted = SmallVec::<[Pointer<T, V>; LOCKBOX_LEN]>::new();
            let mut operate = self.operate_and_purge();
            todo!();
            // for slot in &mut lockbox.write()[..] {
            //     if let Some(arc) = slot.take_if(|a| f(Pointer::ref_cast(a))) {
            //         let pointer = Pointer(arc);
            //         operate.remove(&pointer);
            //         extracted.push(pointer);
            //     }
            //     todo!("eager delete")
            // }
            extracted
        })
    }

    fn insert(&self, value: T) -> Self::Pointer {
        let mut operate = self.operate_and_purge();
        let key = value.key();
        let mut probe = self.probe(key);

        while let Some(lockbox) = probe.next_lockbox() {
            let mut guard = lockbox.write();
            while let Some(offset) = probe.next_offset() {
                let slot = &mut guard[offset];
                if slot.as_ref().is_none_or(|p| p.key() == key) {
                    let pointer = Pointer::new(value, &mut operate);
                    operate.complete_insert(&pointer);
                    if let Some(prev) = slot.replace(pointer.clone()) {
                        let _ = operate.remove(&prev);
                    }
                    return pointer;
                }
            }
        }

        panic!("exceeded capacity")
    }

    fn remove(&self, value: &T) -> Option<Self::Pointer> {
        self.remove_key_if(value.key(), |v| std::ptr::addr_eq(value, &**v))
            .ok()
            .flatten()
    }

    fn upsert(
        &self,
        value: T,
        mut f: impl for<'a> FnMut(&'a T, &'a Self::Pointer) -> &'a T,
    ) -> Self::Pointer {
        let mut operate = self.operate_and_purge();
        let key = value.key();
        let mut probe = self.probe(key);

        while let Some(lockbox) = probe.next_lockbox() {
            let guard = lockbox.upgradable_read();
            while let Some(offset) = probe.next_offset() {
                let slot = &guard[offset];
                match &slot {
                    Some(existing) if existing.key() == key => {
                        if operate.start_read(existing) == StartRead::Allow
                            && std::ptr::eq(&**existing, f(&value, existing))
                        {
                            operate.complete_read(existing);
                            return existing.clone();
                        } else {
                            let pointer = Pointer::new(value, &mut operate);
                            {
                                let mut guard = RwLockUpgradableReadGuard::upgrade(guard);
                                let _ = operate.remove(guard[offset].as_ref().unwrap());
                                guard[offset] = Some(pointer.clone());
                            }
                            operate.complete_insert(&pointer);

                            return pointer;
                        }
                    }
                    Some(_) => {}
                    None => {
                        let pointer = Pointer::new(value, &mut operate);
                        {
                            let mut guard = RwLockUpgradableReadGuard::upgrade(guard);
                            guard[offset] = Some(pointer.clone());
                        }
                        operate.complete_insert(&pointer);
                        return pointer;
                    }
                }
            }
        }

        panic!("exceeded capacity")
    }
}

impl<T, L, V, H> crate::store::hash::Store<T> for FixedCapSyncHashStore<T, L, V, H>
where
    T: super::Value + Send + Sync + 'static,
    L: Layer<Pointer<T, V>, Value = V>,
    V: Send + Sync + 'static,
    H: BuildHasher,
{
    fn entry<'a, K>(
        &'a self,
        key: &K,
    ) -> Entry<
        impl super::OccupiedEntry<'a, Value = T, Pointer = Self::Pointer> + use<'a, T, L, V, H, K>,
        impl super::VacantEntry<'a, Value = T, Pointer = Self::Pointer> + use<'a, T, L, V, H, K>,
    >
    where
        K: ?Sized + Hash + Equivalent<T::Key>,
    {
        let mut operate = self.operate_and_purge();
        let mut probe = self.probe(key);

        while let Some(lockbox) = probe.next_lockbox() {
            let guard = lockbox.read();
            while let Some(offset) = probe.next_offset() {
                match &guard[offset] {
                    Some(existing) if key.equivalent(existing.key()) => {
                        match operate.start_read(existing) {
                            StartRead::Allow => {
                                operate.complete_read(existing);
                                return Entry::Occupied(OccupiedEntry {
                                    store: self,
                                    probe,
                                    guard: guard.into(),
                                });
                            }
                            StartRead::Remove => {
                                let existing = existing.clone();
                                drop(guard);
                                let guard = lockbox.write();
                                return match &guard[offset] {
                                    Some(p) if Arc::ptr_eq(&p.0, &existing.0) => todo!(),
                                    Some(_) => Entry::Occupied(OccupiedEntry {
                                        store: self,
                                        probe,
                                        guard: guard.into(),
                                    }),
                                    None => Entry::Vacant(VacantEntry {
                                        store: self,
                                        probe,
                                        guard: guard.into(),
                                    }),
                                };
                            }
                        }
                    }
                    Some(_) => {}
                    None => {
                        return Entry::Vacant(VacantEntry {
                            store: self,
                            probe,
                            guard: guard.into(),
                        });
                    }
                }
            }
        }

        panic!("exceeded capacity")
    }

    fn or_insert_with<K>(&self, key: K, value: impl FnOnce(K) -> T) -> Self::Pointer
    where
        K: Hash + Equivalent<<T as super::Value>::Key>,
    {
        todo!()
    }

    fn remove_key_if(
        &self,
        key: &(impl ?Sized + Hash + Equivalent<T::Key>),
        mut f: impl FnMut(&Self::Pointer) -> bool,
    ) -> Result<Option<Self::Pointer>, Self::Pointer> {
        todo!()
    }
}

pub struct OccupiedEntry<'a, T, L, V, H = DefaultHashBuilder> {
    store: &'a FixedCapSyncHashStore<T, L, V, H>,
    probe: Probe<'a, T, V>,
    guard: LockboxGuard<'a, T, V>,
}

impl<'a, T, L, V, H> crate::store::hash::OccupiedEntry<'a> for OccupiedEntry<'a, T, L, V, H>
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
        &**self.pointer()
    }

    fn pointer(&self) -> &Self::Pointer {
        self.guard[self.probe.offset()].as_ref().unwrap()
    }

    fn into_pointer(self) -> Self::Pointer {
        self.pointer().clone()
    }

    fn try_remove(self) -> Result<Self::Pointer, Entry<Self, Self::VacantEntry>> {
        // XX match on lock guard type

        let expected = self.pointer().clone();
        drop(self.guard);

        let mut guard = self.probe.lockbox().write();
        match &mut guard[self.probe.offset()] {
            Some(existing) if Arc::ptr_eq(&expected.0, &existing.0) => {
                let mut operate = self.store.layer.operate();
                guard[self.probe.offset()] = None;
                match operate.remove(&expected) {
                    Remove::Allow => todo!(),
                    Remove::Hide => todo!(),
                }
                Ok(expected)
            }
            Some(existing) => {
                let mut operate = self.store.layer.operate();
                match operate.start_read(existing) {
                    StartRead::Allow => {
                        operate.complete_read(existing);
                        Err(Entry::Occupied(OccupiedEntry {
                            store: self.store,
                            probe: self.probe,
                            guard: guard.into(),
                        }))
                    }
                    StartRead::Remove => {
                        todo!("eager delete")
                    }
                }
            }
            None => Err(Entry::Vacant(VacantEntry {
                store: self.store,
                probe: self.probe,
                guard: guard.into(),
            })),
        }
    }

    fn insert(self, value: Self::Value) -> Self::Pointer {
        drop(self.guard);

        let mut operate = self.store.layer.operate();
        let pointer = Pointer::new(value, &mut operate);

        let mut guard = self.probe.lockbox().write();
        if let Some(existing) = guard[self.probe.offset()].replace(pointer.clone()) {
            let _ = operate.remove(&existing);
        }
        operate.complete_insert(&pointer);

        pointer
    }

    fn remove_key(self) -> Option<Self::Pointer> {
        drop(self.guard);

        let mut guard = self.probe.lockbox().write();
        let mut operate = self.store.layer.operate();
        let pointer = guard[self.probe.offset()].take()?;

        let can_see = operate.remove(&pointer) == Remove::Allow;
        todo!("eager delete");

        if can_see { Some(pointer) } else { None }
    }

    fn try_insert(
        self,
        value: Self::Value,
    ) -> Result<Self::Pointer, (Self::Value, Entry<Self, Self::VacantEntry>)> {
        let (mut guard, expected) = match &self.guard {
            LockboxGuard::Read(_) => {
                let existing = self.guard[self.probe.offset()].clone().unwrap();
                drop(self.guard);
                (self.probe.lockbox().write(), existing)
            }
            LockboxGuard::Write(_) => return Ok(self.insert(value)),
        };

        match &mut guard[self.probe.offset()] {
            Some(p) if Arc::ptr_eq(&expected.0, &p.0) => {
                let mut operate = self.store.layer.operate();
                let pointer = Pointer::new(value, &mut operate);
                guard[self.probe.offset()] = Some(pointer.clone());
                Ok(pointer)
            }
            Some(existing) => {
                let mut operate = self.store.layer.operate();
                match operate.start_read(existing) {
                    StartRead::Allow => {
                        operate.complete_read(existing);
                        Err((
                            value,
                            Entry::Occupied(OccupiedEntry {
                                store: self.store,
                                probe: self.probe,
                                guard: guard.into(),
                            }),
                        ))
                    }
                    StartRead::Remove => {
                        todo!("eager delete")
                    }
                }
            }
            None => Err((
                value,
                Entry::Vacant(VacantEntry {
                    store: self.store,
                    probe: self.probe,
                    guard: guard.into(),
                }),
            )),
        }
    }
}

pub struct VacantEntry<'a, T, L, V, H = DefaultHashBuilder> {
    store: &'a FixedCapSyncHashStore<T, L, V, H>,
    probe: Probe<'a, T, V>,
    guard: LockboxGuard<'a, T, V>,
}

impl<'a, T, L, V, H> crate::store::hash::VacantEntry<'a> for VacantEntry<'a, T, L, V, H>
where
    T: super::Value + Send + Sync + 'static,
    L: Layer<Pointer<T, V>, Value = V>,
    V: Send + Sync + 'static,
    H: BuildHasher,
{
    type Value = T;
    type Pointer = Pointer<T, V>;
    type OccupiedEntry = OccupiedEntry<'a, T, L, V, H>;

    fn insert(self, value: Self::Value) -> Self::Pointer {
        let mut guard = LockboxGuard::into_write(self.guard);
        let mut operate = self.store.layer.operate();
        let pointer = Pointer::new(value, &mut operate);
        if let Some(existing) = guard[self.probe.offset()].replace(pointer.clone()) {
            let _ = operate.remove(&existing);
        }
        operate.complete_insert(&pointer);
        pointer
    }

    fn try_insert(
        self,
        value: Self::Value,
    ) -> Result<Self::Pointer, (Self::Value, Self::OccupiedEntry)> {
        match &self.guard {
            LockboxGuard::Read(_) => {
                drop(self.guard);

                let mut operate = self.store.layer.operate();
                let mut guard = self.probe.lockbox().write();
                match &guard[self.probe.offset()] {
                    Some(pointer) => match operate.start_read(pointer) {
                        StartRead::Allow => {
                            operate.complete_read(pointer);
                            Err((
                                value,
                                OccupiedEntry {
                                    store: self.store,
                                    probe: self.probe,
                                    guard: guard.into(),
                                },
                            ))
                        }
                        StartRead::Remove => {
                            todo!()
                        }
                    },
                    None => {
                        let pointer = Pointer::new(value, &mut operate);
                        guard[self.probe.offset()] = Some(pointer.clone());
                        operate.complete_insert(&pointer);
                        Ok(pointer)
                    }
                }
            }
            LockboxGuard::Write(_) => Ok(self.insert(value)),
        }
    }
}

impl<T, L, V, H> FixedCapSyncHashStore<T, L, V, H>
where
    T: super::Value + Send + Sync + 'static,
    L: Layer<Pointer<T, V>, Value = V>,
    V: Send + Sync + 'static,
    H: BuildHasher,
{
    fn operate_and_purge<'a>(&'a self) -> impl Operate<Pointer<T, V>> + use<'a, T, L, V, H> {
        struct Purger<'a, T, L, V, H>(&'a FixedCapSyncHashStore<T, L, V, H>);

        impl<T, L, V, H> Purge<'_, Pointer<T, V>> for Purger<'_, T, L, V, H>
        where
            T: super::Value + Send + Sync + 'static,
            L: Layer<Pointer<T, V>, Value = V>,
            V: Send + Sync + 'static,
            H: BuildHasher,
        {
            fn try_remove(&mut self, pointer: &Pointer<T, V>) -> Result<(), ()> {
                let key = pointer.key();
                let mut probe = self.0.probe(key);

                while let Some(lockbox) = probe.next_lockbox() {
                    let mut guard = lockbox.write();
                    while let Some(offset) = probe.next_offset() {
                        match &guard[offset] {
                            Some(p) if Arc::ptr_eq(&p.0, &pointer.0) => {
                                guard[offset] = None;

                                // happy case
                                if guard.get(offset + 1).is_some_and(|slot| slot.is_none()) {
                                    return Ok(());
                                } else {
                                    unimplemented!("eager delete")
                                }
                            }
                            Some(_) => {}
                            None => return Err(()),
                        }
                    }
                }

                panic!("exceeded capacity")
            }
        }

        let mut operate = self.layer.operate();
        operate.purge(Purger(self));
        operate
    }

    fn probe<'a, K: ?Sized + Hash>(&'a self, key: &K) -> Probe<'a, T, V> {
        let hash = self.hasher.hash_one(key) as usize;

        let start_index = hash / LOCKBOX_LEN;
        debug_assert!(LOCKBOX_LEN <= u8::MAX.into());
        let start_offset = (hash % LOCKBOX_LEN) as u8;

        Probe {
            table: &self.table,
            start_index,
            start_offset,
            exhausted_indexes: false,
            index: start_index,
            offset: start_offset,
        }
    }

    fn clean_up_delete(
        &self,
        probe: &mut Probe<'_, T, V>,
        guard: &mut RwLockWriteGuard<'_, LockboxArray<T, V>>,
    ) {
        // happy case, no clean up required or in the same lockbox
        let mut hole_offset = probe.offset();
        let mut hole_index = probe.index();
        debug_assert!(guard[hole_offset].is_none());

        while let Some(offset) = probe.next_offset() {
            match &guard[offset] {
                Some(pointer) => {
                    let candidate_probe = self.probe(pointer.key());
                    if candidate_probe.index() == probe.index()
                        && (hole_offset..offset).contains(&candidate_probe.offset())
                    {
                        guard[hole_offset] = guard[offset].take();
                        hole_offset = offset;
                    }
                }
                None => return,
            }
        }

        // slow case
        let mut guards = SmallVec::<[RwLockWriteGuard<LockboxArray<T, V>>; 8]>::new();
        while let Some(lockbox) = probe.next_lockbox() {
            let mut guard = lockbox.write();
            while let Some(offset) = probe.next_offset() {
                match &guard[offset] {
                    Some(pointer) => {
                        let candidate_probe = self.probe(pointer.key());
                        // if
                        // if candidate_probe.index() == probe.index()
                        //     && (hole_offset..offset).contains(&candidate_probe.offset())
                        // {
                        //     guard[hole_offset] = guard[offset].take();
                        //     hole_offset = offset;
                        // }
                    }
                    None => return,
                }
            }

            guards.push(guard);
        }
    }
}

const CACHE_ROW: usize = mem::align_of::<CachePadded<()>>();
const LOCKBOX_LEN: usize =
    (CACHE_ROW - mem::size_of::<RawRwLock>()) / mem::size_of::<Option<Arc<()>>>();
type LockboxArray<T, V> = [Option<Pointer<T, V>>; LOCKBOX_LEN];

type Lockbox<T, V> = CachePadded<RwLock<LockboxArray<T, V>>>;

const fn empty_lockbox<T, V>() -> Lockbox<T, V> {
    CachePadded::new(RwLock::new([const { None }; LOCKBOX_LEN]))
}

struct Probe<'a, T, V> {
    table: &'a [Lockbox<T, V>],
    start_index: usize,
    start_offset: u8,
    exhausted_indexes: bool,
    index: usize,
    offset: u8,
}

impl<'a, T, V> Probe<'a, T, V> {
    fn index(&self) -> usize {
        self.index
    }

    fn lockbox(&self) -> &'a Lockbox<T, V> {
        &self.table[self.index]
    }

    fn offset(&self) -> usize {
        self.offset.into()
    }

    fn next_index(&mut self) -> Option<usize> {
        if self.exhausted_indexes {
            None
        } else {
            let result = next_wrapping(&mut self.index, self.table.len());
            self.exhausted_indexes = result == self.start_index;
            Some(result)
        }
    }

    fn next_lockbox(&mut self) -> Option<&'a Lockbox<T, V>> {
        self.next_index().map(|i| &self.table[i])
    }

    fn next_offset(&mut self) -> Option<usize> {
        if self.exhausted_indexes && self.offset >= self.start_offset {
            None
        } else {
            Some(next_wrapping(&mut self.offset, LOCKBOX_LEN as u8).into())
        }
    }
}

fn next_wrapping<N: From<u8> + Ord + AddAssign + Copy>(index: &mut N, cap: N) -> N {
    if *index >= cap {
        *index = 1u8.into();
        0u8.into()
    } else {
        let result = *index;
        *index += 1.into();
        result
    }
}

enum LockboxGuard<'a, T, V> {
    Read(RwLockReadGuard<'a, LockboxArray<T, V>>),
    Write(RwLockWriteGuard<'a, LockboxArray<T, V>>),
}

impl<T, V> Deref for LockboxGuard<'_, T, V> {
    type Target = LockboxArray<T, V>;

    fn deref(&self) -> &Self::Target {
        match self {
            LockboxGuard::Read(guard) => &*guard,
            LockboxGuard::Write(guard) => &*guard,
        }
    }
}

impl<'a, T, V> From<RwLockReadGuard<'a, LockboxArray<T, V>>> for LockboxGuard<'a, T, V> {
    fn from(value: RwLockReadGuard<'a, LockboxArray<T, V>>) -> Self {
        Self::Read(value)
    }
}

impl<'a, T, V> From<RwLockWriteGuard<'a, LockboxArray<T, V>>> for LockboxGuard<'a, T, V> {
    fn from(value: RwLockWriteGuard<'a, LockboxArray<T, V>>) -> Self {
        Self::Write(value)
    }
}

impl<'a, T, V> LockboxGuard<'a, T, V> {
    fn into_write(guard: Self) -> RwLockWriteGuard<'a, LockboxArray<T, V>> {
        match guard {
            LockboxGuard::Read(read) => {
                let lockbox = RwLockReadGuard::rwlock(&read);
                drop(read);
                lockbox.write()
            }
            LockboxGuard::Write(write) => write,
        }
    }
}
