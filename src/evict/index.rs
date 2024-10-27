use std::{num::NonZero, sync::atomic::{AtomicU64, Ordering}};


type IndexRepr = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct Index(NonZero<IndexRepr>);

impl Index {
    pub const MAX: Self = Self(NonZero::<IndexRepr>::MAX);

    pub fn into_usize(self) -> usize {
        self.into()
    }
}

impl From<usize> for Index {
    fn from(index: usize) -> Self {
        let bumped = index.checked_add(1).unwrap();
        let as_repr = IndexRepr::try_from(bumped).unwrap();
        Self(NonZero::new(as_repr).unwrap())
    }
}

impl From<Index> for usize {
    fn from(index: Index) -> Self {
        let index = index.0.get() - 1;
        index.try_into().unwrap()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct Generation(NonZero<u32>);

impl Generation {
    pub const fn initial() -> Self {
        Self(NonZero::<u32>::MIN)
    }

    pub fn increment(self) -> Self {
        self.0.checked_add(1).map(Self).unwrap_or(Self::initial())
    }

    pub fn increment_mut(&mut self) {
        *self = self.increment();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[doc(hidden)]
pub struct Key {
    pub(super) index: Index,
    pub(super) gen: Generation,
}

#[derive(Debug)]
#[doc(hidden)]
pub struct AtomicKey(AtomicU64);

fn key_to_u64(key: Key) -> u64 {
    let mut as_bytes = [0u8; 8];
    as_bytes[..4].copy_from_slice(&key.index.0.get().to_ne_bytes());
    as_bytes[4..].copy_from_slice(&key.gen.0.get().to_ne_bytes());
    u64::from_ne_bytes(as_bytes)
}

fn u64_to_key(value: u64) -> Key {
    let as_bytes = value.to_ne_bytes();
    Key {
        gen: Generation(NonZero::new(u32::from_be_bytes((&as_bytes[..4]).try_into().unwrap())).unwrap()),
        index: Index(NonZero::new(u32::from_be_bytes((&as_bytes[4..]).try_into().unwrap())).unwrap()),
    }
}

impl From<Key> for AtomicKey {
    fn from(value: Key) -> Self {
        Self(AtomicU64::new(key_to_u64(value)))
    }
}

impl AtomicKey {
    pub fn get(&mut self) -> Key {
        u64_to_key(*self.0.get_mut())
    }

    pub fn load(&self, order: Ordering) -> Key {
        u64_to_key(self.0.load(order))
    }

    pub fn store(&self, key: Key, order: Ordering) {
        self.0.store(key_to_u64(key), order);
    }
}

