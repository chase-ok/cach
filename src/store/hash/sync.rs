use std::{
    hash::{BuildHasher, Hash},
    mem,
    ops::Deref,
    sync::Arc,
};

use crossbeam_utils::CachePadded;
use equivalent::Equivalent;
use hashbrown::DefaultHashBuilder;
use parking_lot::{RawRwLock, RwLock, RwLockReadGuard, RwLockWriteGuard};
use smallvec::{SmallVec, smallvec};
use stable_deref_trait::{CloneStableDeref, StableDeref};

use crate::{
    store::{
        Entry,
        hash::{OccupiedEntry as _, Store, VacantEntry as _},
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
            .div_ceil(1 << OFFSET_BITS)
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
        Iter {
            should_remove: None::<fn(&Pointer<T, V>) -> bool>,
            probe: Probe::new(self, Cursor(0)),
            items: SmallVec::new()
        }
    }

    fn extract_if<'a>(
        &'a self,
        f: impl FnMut(&Self::Pointer) -> bool + 'a,
    ) -> impl Iterator<Item = Self::Pointer> + 'a {
        Iter {
            should_remove: Some(f),
            probe: Probe::new(self, Cursor(0)),
            items: SmallVec::new()
        }
    }

    fn insert(&self, value: T) -> Self::Pointer {
        let mut operate = self.operate_and_purge();
        match self
            .probe(value.key())
            .entry_write(value.key(), &mut operate)
        {
            Entry::Occupied(OccupiedEntry { probe, guard, .. })
            | Entry::Vacant(VacantEntry { probe, guard, .. }) => {
                let (pointer, _removed) =
                    probe.insert(&mut guard.assert_write(), &mut operate, value);
                pointer
            }
        }
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

        // Try read first in happy case of no write required
        let mut probe = match self
            .probe(value.key())
            .entry_read(value.key(), &mut operate)
        {
            Entry::Occupied(o) if std::ptr::addr_eq(f(&value, o.pointer()), o.get()) => {
                return o.into_pointer();
            }

            Entry::Occupied(OccupiedEntry {
                probe,
                guard: LockboxGuard::Write(mut guard),
                ..
            })
            | Entry::Vacant(VacantEntry {
                probe,
                guard: LockboxGuard::Write(mut guard),
                ..
            }) => {
                let (pointer, _removed) = probe.insert(&mut guard, &mut operate, value);
                return pointer;
            }

            Entry::Occupied(o) => o.probe,
            Entry::Vacant(v) => v.probe,
        };

        probe.reset();
        match probe.entry_write(value.key(), &mut operate) {
            Entry::Occupied(o) if std::ptr::addr_eq(f(&value, o.pointer()), o.get()) => {
                o.into_pointer()
            }

            Entry::Occupied(OccupiedEntry { probe, guard, .. })
            | Entry::Vacant(VacantEntry { probe, guard, .. }) => {
                let (pointer, _removed) =
                    probe.insert(&mut guard.assert_write(), &mut operate, value);
                pointer
            }
        }
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
        key: &'a K,
    ) -> Entry<
        impl super::OccupiedEntry<'a, Value = T, Pointer = Self::Pointer> + use<'a, T, L, V, H, K>,
        impl super::VacantEntry<'a, Value = T, Pointer = Self::Pointer> + use<'a, T, L, V, H, K>,
    >
    where
        K: ?Sized + Hash + Equivalent<T::Key>,
    {
        let mut operate = self.operate_and_purge();
        self.probe(key).entry_read(key, &mut operate)
    }

    fn or_insert_with<K>(&self, key: K, value: impl FnOnce(K) -> T) -> Self::Pointer
    where
        K: Hash + Equivalent<T::Key>,
    {
        let mut operate = self.operate_and_purge();
        let (probe, mut guard) = match self.probe(&key).entry_read(&key, &mut operate) {
            Entry::Occupied(o) => return o.into_pointer(),

            // the layer might've removed a value with the same key!
            Entry::Vacant(VacantEntry {
                probe,
                guard: LockboxGuard::Write(guard),
                ..
            }) => (probe, guard),

            Entry::Vacant(VacantEntry {
                mut probe, guard, ..
            }) => {
                drop(guard);

                // We need to reset the probe because the right slot for the
                // current key could be behind the probe!
                probe.reset();
                match probe.entry_write(&key, &mut operate) {
                    Entry::Occupied(o) => return o.into_pointer(),
                    Entry::Vacant(VacantEntry { probe, guard, .. }) => {
                        (probe, guard.assert_write())
                    }
                }
            }
        };

        probe.insert_assert_vacant(&mut guard, &mut operate, value(key))
    }

    fn remove_key_if(
        &self,
        key: &(impl ?Sized + Hash + Equivalent<T::Key>),
        mut f: impl FnMut(&Self::Pointer) -> bool,
    ) -> Result<Option<Self::Pointer>, Self::Pointer> {
        let mut operate = self.operate_and_purge();
        match self.probe(key).entry_write(key, &mut operate) {
            Entry::Occupied(o) if f(o.pointer()) => Ok(Some(o.remove_write(&mut operate))),
            Entry::Occupied(o) => Err(o.into_pointer()),
            Entry::Vacant(_) => Ok(None),
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

                let mut prev = probe.current;
                let mut guard = None;
                while let Some(cursor) = probe.next() {
                    if cursor.index() != prev.index() {
                        guard = None;
                    }

                    let guard_ref = guard.get_or_insert_with(|| probe.lockbox().write());
                    match &guard_ref[cursor.offset()] {
                        Some(p) if Arc::ptr_eq(&p.0, &pointer.0) => {
                            let _ = probe.vacate(guard.unwrap());
                            return Ok(());
                        }
                        Some(_) => {}
                        None => return Err({}),
                    }

                    prev = cursor;
                }

                panic!("exceeded capacity")
            }
        }

        let mut operate = self.layer.operate();
        operate.purge(Purger(self));
        operate
    }

    fn cursor(&self, key: &(impl ?Sized + Hash)) -> Cursor {
        let hash = self.hasher.hash_one(key) as usize;

        let index = (hash >> OFFSET_BITS) & self.mask;
        let offset = hash & OFFSET_MASK;
        // XX: 1/16 bias towards the first slot to avoid division, reduce
        // multi-lockbox lookups
        Cursor::new(
            index,
            if offset >= LOCKBOX_LEN {
                (offset - LOCKBOX_LEN) as u8
            } else {
                offset as u8
            },
        )
    }

    fn probe<'a, K: ?Sized + Hash>(&'a self, key: &K) -> Probe<'a, T, L, V, H> {
        let start = self.cursor(key);
        Probe::new(self, start)
    }
}

struct Iter<'a, F, T, L, V, H> {
    should_remove: Option<F>,
    probe: Probe<'a, T, L, V, H>,
    items: SmallVec<[Pointer<T, V>; LOCKBOX_LEN]>,
}

impl<F, T, L, V, H> Iterator for Iter<'_, F, T, L, V, H>
where
    F: FnMut(&Pointer<T, V>) -> bool,
    T: super::Value + Send + Sync + 'static,
    L: Layer<Pointer<T, V>, Value = V>,
    V: Send + Sync + 'static,
    H: BuildHasher,
{
    type Item = Pointer<T, V>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(item) = self.items.pop() {
                return Some(item);
            }

            let start = self.probe.next()?;
            let mut operate = self.probe.store.layer.operate();
            let mut guard = self.probe.lockbox().write();
            loop {
                if let Some(pointer) = &guard[self.probe.offset()] {
                    match operate.start_read(pointer) {
                        StartRead::Allow => {
                            if let Some(should_remove) = self.should_remove.as_mut() {
                                if should_remove(pointer) {
                                    let _ = operate.remove(pointer);
                                    self.items.push(pointer.clone());
                                    // XX we need to inline the loop here ourselves :/
                                    guard = self.probe.vacate(guard);
                                    todo!()
                                }
                            } else {
                                // XX no need to call complete_read() on all items?
                                self.items.push(pointer.clone());
                            }
                        }
                        StartRead::Remove => todo!(),
                    }
                }

                if self.probe.next().is_none_or(|n| n.index() != start.index()) {
                    break;
                }
            }
        }
    }
}

pub struct OccupiedEntry<'a, 'k, K: ?Sized, T, L, V, H = DefaultHashBuilder> {
    probe: Probe<'a, T, L, V, H>,
    key: &'k K,
    guard: LockboxGuard<'a, T, V>,
}

impl<'a, 'k, K, T, L, V, H> crate::store::hash::OccupiedEntry<'k>
    for OccupiedEntry<'a, 'k, K, T, L, V, H>
where
    'a: 'k,
    K: ?Sized + Equivalent<T::Key>,
    T: super::Value + Send + Sync + 'static,
    L: Layer<Pointer<T, V>, Value = V>,
    V: Send + Sync + 'static,
    H: BuildHasher,
{
    type Value = T;
    type Pointer = Pointer<T, V>;
    type VacantEntry = VacantEntry<'a, 'k, K, T, L, V, H>;

    fn get(&self) -> &Self::Value {
        &**self.pointer()
    }

    fn pointer(&self) -> &Self::Pointer {
        self.guard[self.probe.offset()].as_ref().unwrap()
    }

    fn into_pointer(self) -> Self::Pointer {
        self.pointer().clone()
    }

    fn try_remove(mut self) -> Result<Self::Pointer, Entry<Self, Self::VacantEntry>> {
        match &self.guard {
            LockboxGuard::Read(_) => {
                let existing = self.pointer().clone();
                drop(self.guard);

                // Happy case: the slot still has the same
                // pointer (no A B A problem because we still
                // hold a ref in existing!)
                let guard = self.probe.lockbox().write();
                let mut operate = self.probe.store.layer.operate();
                if guard[self.probe.offset()]
                    .as_ref()
                    .is_some_and(|p| Arc::ptr_eq(&p.0, &existing.0))
                {
                    let _ = self.probe.vacate(guard);
                    let _ = operate.remove(&existing);
                    return Ok(existing);
                }

                // We need to reset the probe because the right slot for the
                // current key could be behind the probe!
                let guard = if self.probe.start.index() == self.probe.current.index() {
                    guard
                } else {
                    drop(guard);
                    self.probe.store.table[self.probe.start.index()].write()
                };
                self.probe.reset();

                match self
                    .probe
                    .entry_write_with_guard(self.key, &mut operate, guard)
                {
                    Entry::Occupied(o) if Arc::ptr_eq(&o.pointer().0, &existing.0) => {
                        Ok(o.remove_write(&mut operate))
                    }
                    entry => Err(entry),
                }
            }

            LockboxGuard::Write(_) => {
                let mut operate = self.probe.store.layer.operate();
                Ok(self.remove_write(&mut operate))
            }
        }
    }

    fn try_insert(
        mut self,
        value: Self::Value,
    ) -> Result<Self::Pointer, (Self::Value, Entry<Self, Self::VacantEntry>)> {
        match &self.guard {
            LockboxGuard::Read(_) => {
                let existing = self.pointer().clone();
                drop(self.guard);

                // Happy case: the slot still has the same
                // pointer (no A B A problem because we still
                // hold a ref in existing!)
                let mut guard = self.probe.lockbox().write();
                let mut operate = self.probe.store.layer.operate();
                if guard[self.probe.offset()]
                    .as_ref()
                    .is_some_and(|p| Arc::ptr_eq(&p.0, &existing.0))
                {
                    let pointer = Pointer::new(value, &mut operate);
                    let removed = guard[self.probe.offset()].replace(pointer.clone());
                    let _ = operate.remove(&removed.unwrap());
                    operate.complete_insert(&pointer);
                    return Ok(pointer);
                }

                // We need to reset the probe because the right slot for the
                // current key could be behind the probe!
                let guard = if self.probe.start.index() == self.probe.current.index() {
                    guard
                } else {
                    drop(guard);
                    self.probe.store.table[self.probe.start.index()].write()
                };
                self.probe.reset();

                match self
                    .probe
                    .entry_write_with_guard(self.key, &mut operate, guard)
                {
                    Entry::Occupied(o) if Arc::ptr_eq(&o.pointer().0, &existing.0) => {
                        Ok(o.insert_write(value))
                    }
                    entry => Err((value, entry)),
                }
            }

            LockboxGuard::Write(_) => Ok(self.insert_write(value)),
        }
    }
}

impl<'a, 'k, K, T, L, V, H> OccupiedEntry<'a, 'k, K, T, L, V, H>
where
    'a: 'k,
    K: ?Sized + Equivalent<T::Key>,
    T: super::Value + Send + Sync + 'static,
    L: Layer<Pointer<T, V>, Value = V>,
    V: Send + Sync + 'static,
    H: BuildHasher,
{
    fn insert_write(self, value: T) -> Pointer<T, V> {
        let mut guard = self.guard.assert_write();
        let mut operate = self.probe.store.layer.operate();
        self.probe
            .insert_assert_vacant(&mut guard, &mut operate, value)
    }

    fn remove_write(mut self, operate: &mut impl Operate<Pointer<T, V>>) -> Pointer<T, V> {
        let removed = self.pointer().clone();
        let _ = self.probe.vacate(self.guard.assert_write());
        let _ = operate.remove(&removed);
        removed
    }
}

pub struct VacantEntry<'a, 'k, K: ?Sized, T, L, V, H = DefaultHashBuilder> {
    probe: Probe<'a, T, L, V, H>,
    key: &'k K,
    guard: LockboxGuard<'a, T, V>,
}

impl<'a, 'k, K, T, L, V, H> crate::store::hash::VacantEntry<'k>
    for VacantEntry<'a, 'k, K, T, L, V, H>
where
    'a: 'k,
    K: ?Sized + Equivalent<T::Key>,
    T: super::Value + Send + Sync + 'static,
    L: Layer<Pointer<T, V>, Value = V>,
    V: Send + Sync + 'static,
    H: BuildHasher,
{
    type Value = T;
    type Pointer = Pointer<T, V>;
    type OccupiedEntry = OccupiedEntry<'a, 'k, K, T, L, V, H>;

    fn try_insert(
        mut self,
        value: Self::Value,
    ) -> Result<Self::Pointer, (Self::Value, Self::OccupiedEntry)> {
        debug_assert!(self.key.equivalent(value.key()));

        let mut operate = self.probe.store.layer.operate();
        let (mut guard, probe) = match self.guard {
            LockboxGuard::Read(guard) => {
                drop(guard);

                // We need to reset the probe because the right slot for the
                // current key could be behind the probe!
                self.probe.reset();
                match self.probe.entry_write(self.key, &mut operate) {
                    Entry::Occupied(o) => return Err((value, o)),
                    Entry::Vacant(v) => (v.guard.assert_write(), v.probe),
                }
            }

            LockboxGuard::Write(guard) => (guard, self.probe),
        };

        Ok(probe.insert_assert_vacant(&mut guard, &mut operate, value))
    }
}

const CACHE_ROW: usize = mem::align_of::<CachePadded<()>>();
const LOCK_SIZE: usize = mem::size_of::<RawRwLock>();
const ITEM_SIZE: usize = mem::size_of::<Option<Arc<()>>>();
const LOCKBOX_LEN: usize = (CACHE_ROW - LOCK_SIZE) / ITEM_SIZE;

const OFFSET_BITS: u32 = usize::BITS - LOCKBOX_LEN.leading_zeros();
const OFFSET_MASK: usize = (1usize << OFFSET_BITS) - 1;

type LockboxArray<T, V> = [Option<Pointer<T, V>>; LOCKBOX_LEN];
type Lockbox<T, V> = CachePadded<RwLock<LockboxArray<T, V>>>;

const _: () = {
    assert!(OFFSET_BITS <= u8::BITS);
    assert!(mem::size_of::<Lockbox<(), ()>>() == CACHE_ROW);
};

const fn empty_lockbox<T, V>() -> Lockbox<T, V> {
    CachePadded::new(RwLock::new([const { None }; LOCKBOX_LEN]))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Cursor(usize);

impl Cursor {
    pub fn new(index: usize, offset: u8) -> Self {
        debug_assert_eq!(index, (index << OFFSET_BITS) >> OFFSET_BITS);
        debug_assert_eq!(usize::from(offset) & !OFFSET_MASK, 0);

        Self(index << u8::BITS & usize::from(offset))
    }

    pub fn offset(self) -> usize {
        self.0 & OFFSET_MASK
    }

    pub fn with_offset(self, offset: usize) -> Self {
        debug_assert!(offset <= LOCKBOX_LEN);
        Self::new(self.index(), offset as u8)
    }

    pub fn index(self) -> usize {
        self.0 >> OFFSET_BITS
    }

    pub fn is_wrapping_between(self, exclusive_lower: Cursor, exclusive_upper: Cursor) -> bool {
        debug_assert_ne!(exclusive_lower, exclusive_upper);

        if exclusive_lower < exclusive_upper {
            exclusive_lower < self && self < exclusive_upper
        } else {
            exclusive_lower < self || self < exclusive_upper
        }
    }

    pub fn next(self, table_len: usize) -> Self {
        let next_offset = self.offset() + 1;
        if next_offset >= LOCKBOX_LEN {
            let next_index = self.index() + 1;
            if next_index >= table_len {
                Self::new(0, 0)
            } else {
                Self::new(next_index, 0)
            }
        } else {
            self.with_offset(next_offset)
        }
    }
}

struct Probe<'a, T, L, V, H> {
    store: &'a FixedCapSyncHashStore<T, L, V, H>,
    start: Cursor,
    current: Cursor,
    passed_start: bool,
}

impl<T, L, V, H> Clone for Probe<'_, T, L, V, H> {
    fn clone(&self) -> Self {
        Self { ..*self }
    }
}

impl<'a, T, L, V, H> Probe<'a, T, L, V, H>
where
    T: super::Value + Send + Sync + 'static,
    L: Layer<Pointer<T, V>, Value = V>,
    V: Send + Sync + 'static,
    H: BuildHasher,
{
    pub fn new(store: &'a FixedCapSyncHashStore<T, L, V, H>, start: Cursor) -> Self {
        Self {
            store,
            start,
            current: start,
            passed_start: false,
        }
    }

    pub fn lockbox(&self) -> &'a Lockbox<T, V> {
        &self.store.table[self.current.index()]
    }

    pub fn offset(&self) -> usize {
        self.current.offset()
    }

    pub fn next(&mut self) -> Option<Cursor> {
        if self.passed_start {
            let next = self.current.next(self.store.table.len());
            if next == self.start {
                None
            } else {
                self.current = next;
                Some(next)
            }
        } else {
            self.passed_start = true;
            debug_assert_eq!(self.current, self.start);
            Some(self.current)
        }
    }

    pub fn reset(&mut self) {
        self.current = self.start;
        self.passed_start = false;
    }

    pub fn distance(&self) -> usize {
        let diff = self.current.0.abs_diff(self.start.0);
        if self.current >= self.start {
            diff
        } else {
            (self.store.table.len() << OFFSET_BITS) - diff
        }
    }

    #[inline(always)]
    pub fn vacate(
        &mut self,
        mut guard: RwLockWriteGuard<'a, LockboxArray<T, V>>,
    ) -> RwLockWriteGuard<'a, LockboxArray<T, V>> {
        debug_assert!(guard[self.offset()].is_some());
        guard[self.offset()] = None;

        // short circuit the happy case where next offset is a hole
        if guard
            .get(self.offset() + 1)
            .is_some_and(|candidate| candidate.is_none())
        {
            guard
        } else {
            self.vacate_slow(guard)
        }
    }

    #[inline(never)]
    fn vacate_slow(
        &mut self,
        guard: RwLockWriteGuard<'a, LockboxArray<T, V>>,
    ) -> RwLockWriteGuard<'a, LockboxArray<T, V>> {
        let mut guards: SmallVec<[_; 7]> = smallvec![guard];
        let mut candidate_guard_index = 0;
        let mut hole = self.current;
        let mut prev = self.current;

        while let Some(cursor) = self.next() {
            if cursor.index() != prev.index() {
                if guards.len() < self.store.table.len() {
                    guards.push(self.lockbox().write());
                    candidate_guard_index = guards.len() - 1;
                } else {
                    // we've wrapped back around to the first lockbox we looked at!
                    candidate_guard_index = 0;
                }
            }

            let Some(candidate) = &guards[candidate_guard_index][cursor.offset()] else {
                // We've reached an existing hole, we can stop vacating.
                // Point probe back to the hole we vacated.
                self.current = hole;

                // XX is the drop order ok?
                return guards
                    // Drain ensures we drop all the other guards no
                    // matter where we find the hole guard (the hole
                    // could be in the first slot of the next lockbox)
                    .drain(..)
                    .next()
                    .unwrap();
            };

            let candidate_origin = self.store.cursor(candidate.key());
            if candidate_origin.is_wrapping_between(hole, cursor) {
                if candidate_guard_index == 0 {
                    debug_assert_eq!(hole.index(), self.current.index());
                    guards[0].swap(hole.offset(), cursor.offset());
                } else {
                    let [hole_guard, guard] =
                        guards.get_disjoint_mut([0, candidate_guard_index]).unwrap();
                    hole_guard[hole.offset()] = guard[cursor.offset()].take();
                    hole = cursor;

                    // Drop guards we no longer need!
                    let num_guards_to_drop = candidate_guard_index;
                    guards.drain(..num_guards_to_drop);
                    candidate_guard_index = 0;
                }
            }

            prev = cursor;
        }

        panic!("capacity exceeded")
    }

    pub fn entry_read<'k, K>(
        mut self,
        key: &'k K,
        operate: &mut impl Operate<Pointer<T, V>>,
    ) -> Entry<OccupiedEntry<'a, 'k, K, T, L, V, H>, VacantEntry<'a, 'k, K, T, L, V, H>>
    where
        K: ?Sized + Hash + Equivalent<T::Key>,
    {
        let mut prev = self.current;
        let mut guard: LockboxGuard<T, V> = self.lockbox().read().into();

        while let Some(cursor) = self.next() {
            if cursor.index() != prev.index() {
                drop(guard);
                guard = self.lockbox().read().into();
            }

            match &guard[cursor.offset()] {
                Some(existing) if key.equivalent(existing.key()) => {
                    match operate.start_read(existing) {
                        StartRead::Allow => {
                            operate.complete_read(existing);
                            return Entry::Occupied(OccupiedEntry {
                                probe: self,
                                key,
                                guard,
                            });
                        }

                        StartRead::Remove => match guard {
                            LockboxGuard::Read(_) => {
                                let existing = existing.clone();
                                drop(guard);
                                let mut write = self.lockbox().write();

                                // Happy case: the slot still has the same
                                // pointer (no A B A problem because we still
                                // hold a ref in existing!)
                                if write[cursor.offset()]
                                    .as_ref()
                                    .is_some_and(|p| Arc::ptr_eq(&p.0, &existing.0))
                                {
                                    let _ = operate.remove(&existing);
                                    write = self.vacate(write);
                                    return Entry::Vacant(VacantEntry {
                                        probe: self,
                                        key,
                                        guard: write.into(),
                                    });
                                }

                                // Otherwise, the "right" slot for this key
                                // may be behind the probe. We'll need to
                                // start over.
                                self.reset();
                                guard = write.into();
                            }

                            LockboxGuard::Write(_) => {
                                let _ = operate.remove(existing);
                                let guard = self.vacate(guard.assert_write());
                                return Entry::Vacant(VacantEntry {
                                    probe: self,
                                    key,
                                    guard: guard.into(),
                                });
                            }
                        },
                    }
                }

                Some(_) => {}

                None => {
                    return Entry::Vacant(VacantEntry {
                        probe: self,
                        key,
                        guard,
                    });
                }
            }

            prev = cursor;
        }

        panic!("capacity exceeded")
    }

    pub fn entry_write<'k, K>(
        self,
        key: &'k K,
        operate: &mut impl Operate<Pointer<T, V>>,
    ) -> Entry<OccupiedEntry<'a, 'k, K, T, L, V, H>, VacantEntry<'a, 'k, K, T, L, V, H>>
    where
        K: ?Sized + Equivalent<T::Key>,
    {
        let guard = self.lockbox().write();
        self.entry_write_with_guard(key, operate, guard)
    }

    pub fn entry_write_with_guard<'k, K>(
        mut self,
        key: &'k K,
        operate: &mut impl Operate<Pointer<T, V>>,
        mut guard: RwLockWriteGuard<'a, LockboxArray<T, V>>,
    ) -> Entry<OccupiedEntry<'a, 'k, K, T, L, V, H>, VacantEntry<'a, 'k, K, T, L, V, H>>
    where
        K: ?Sized + Equivalent<T::Key>,
    {
        let mut prev = self.current;

        while let Some(cursor) = self.next() {
            if cursor.index() != prev.index() {
                drop(guard);
                guard = self.lockbox().write();
            }

            match &guard[cursor.offset()] {
                Some(existing) if key.equivalent(existing.key()) => {
                    match operate.start_read(existing) {
                        StartRead::Allow => {
                            operate.complete_read(existing);
                            return Entry::Occupied(OccupiedEntry {
                                probe: self,
                                key,
                                guard: guard.into(),
                            });
                        }

                        StartRead::Remove => {
                            let _ = operate.remove(existing);
                            let guard = self.vacate(guard);
                            return Entry::Vacant(VacantEntry {
                                probe: self,
                                key,
                                guard: guard.into(),
                            });
                        }
                    }
                }

                Some(_) => {}

                None => {
                    return Entry::Vacant(VacantEntry {
                        probe: self,
                        key,
                        guard: guard.into(),
                    });
                }
            }

            prev = cursor;
        }

        panic!("capacity exceeded")
    }

    pub fn insert(
        &self,
        guard: &mut RwLockWriteGuard<'_, LockboxArray<T, V>>,
        operate: &mut impl Operate<Pointer<T, V>>,
        value: T,
    ) -> (Pointer<T, V>, Option<Pointer<T, V>>) {
        debug_assert!(std::ptr::addr_eq(
            RwLockWriteGuard::rwlock(guard),
            &self.store.table[self.current.index()]
        ));

        let pointer = Pointer::new(value, operate);
        let removed = guard[self.offset()].replace(pointer.clone());
        removed.as_ref().inspect(|r| {
            let _ = operate.remove(r);
        });
        operate.complete_insert(&pointer);

        (pointer, removed)
    }

    pub fn insert_assert_vacant(
        &self,
        guard: &mut RwLockWriteGuard<'_, LockboxArray<T, V>>,
        operate: &mut impl Operate<Pointer<T, V>>,
        value: T,
    ) -> Pointer<T, V> {
        let (pointer, removed) = self.insert(guard, operate, value);
        debug_assert!(removed.is_none());
        pointer
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
    fn assert_write(self) -> RwLockWriteGuard<'a, LockboxArray<T, V>> {
        match self {
            LockboxGuard::Read(_) => unreachable!(),
            LockboxGuard::Write(write) => write,
        }
    }
}
