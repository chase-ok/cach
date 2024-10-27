use std::sync::atomic::{AtomicUsize, Ordering};

use crate::evict::index::Index;

pub(crate) struct Bag<T> {
    values: Vec<T>,
}

#[doc(hidden)]
pub struct Key(AtomicUsize);

impl<T> Bag<T> {
    pub fn with_capacity(capacity: usize) -> Self {
        assert!(capacity <= Index::MAX.into_usize());
        Self {
            values: Vec::with_capacity(capacity),
        }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn capacity(&self) -> usize {
        self.values.capacity()
    }

    pub fn insert_with_key(&mut self, construct: impl FnOnce(Key) -> T) -> &T {
        let index = self.values.len();
        self.values.push(construct(Key(index.into())));
        &self.values[index]
    }

    pub fn remove(&mut self, value: &T, deref: impl Fn(&T) -> &Key) -> T {
        let key = deref(value);
        // XX: can use relaxed since we have &mut self
        let index = key.0.load(Ordering::Relaxed);
        self.do_remove(index, deref)
    }

    pub fn pop(&mut self, rand: impl FnOnce(usize) -> usize, deref: impl Fn(&T) -> &Key) -> Option<T> {
        if self.len() == 0 {
            return None
        }

        let index = rand(self.len());
        assert!(index < self.len());
        Some(self.do_remove(index, deref))
    }

    fn do_remove(&mut self, index: usize, deref: impl Fn(&T) -> &Key) -> T {
        let removed = self.values.swap_remove(index);
        if let Some(moved) = self.values.get(index) {
            // XX: can used relaxed
            deref(moved).0.store(index, Ordering::Relaxed);
        }
        removed
    }
}