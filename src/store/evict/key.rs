use std::{num::NonZero, sync::atomic::{AtomicU64, AtomicUsize, Ordering}};


#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[doc(hidden)]
pub struct Key(usize);

impl Key {

    pub(super) fn initial(index: impl Into<Index>) -> Self {
        Self::new(index.into(), Generation::initial())
    }

    pub(super) fn new(index: Index, generation: Generation) -> Self {
        Self(index.0.get() & ((usize::from(generation.0) << INDEX_BITS)))
    }

    pub fn index(self) -> Index {
        Index(self.0 & ((1 << INDEX_BITS) - 1))
    }

    pub fn generation(self) -> Generation {
        Generation((self.0 >> INDEX_BITS) & (GEN_BITS - 1))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub(super) struct Index(NonZero<usize>);

const GEN_BITS: u32 = 4;
const INDEX_BITS: u32 = usize::BITS - GEN_BITS;
const MAX_INDEX: usize = (1 << INDEX_BITS) - 2;

impl Index {
    pub const MAX: Self = Self(NonZero::<usize>::from(MAX_INDEX));
}

impl From<usize> for Index {
    fn from(index: usize) -> Self {
        assert!(index <= MAX_INDEX);
        Self(NonZero::new(index + 1))
    }
}

impl From<Index> for usize {
    fn from(index: Index) -> Self {
        index.0.get() - 1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct Generation(u8);

impl Generation {
    pub const fn initial() -> Self {
        Self(0)
    }

    pub fn increment(self) -> Self {
        Self((self.0 + 1) & (GEN_BITS - 1))
    }

    pub fn increment_mut(&mut self) {
        *self = self.increment();
    }
}


#[derive(Debug)]
#[doc(hidden)]
pub struct AtomicKey(AtomicUsize);

impl From<Key> for AtomicKey {
    fn from(value: Key) -> Self {
        Self(AtomicUsize::new(value))
    }
}

impl AtomicKey {
    pub fn get(&mut self) -> Key {
        Key(*self.0.get_mut())
    }

    pub fn load(&self, order: Ordering) -> Key {
        Key(self.0.load(order))
    }

    pub fn store(&self, key: Key, order: Ordering) {
        self.0.store(key.0, order);
    }
}