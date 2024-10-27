use std::sync::atomic::{AtomicUsize, Ordering};

use crate::evict::index::Index;

use super::Point;

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

    pub fn remove<Pt: Point<T, Key>>(&mut self, value: &T) -> T {
        let key = Pt::point(value);
        // XX: can use relaxed since we have &mut self
        let index = key.0.load(Ordering::Relaxed);
        self.do_remove::<Pt>(index)
    }

    pub fn pop<Pt: Point<T, Key>>(&mut self, rand: impl FnOnce(usize) -> usize) -> Option<T> {
        if self.len() == 0 {
            return None
        }

        let index = rand(self.len());
        assert!(index < self.len());
        Some(self.do_remove::<Pt>(index))
    }

    fn do_remove<Pt: Point<T, Key>>(&mut self, index: usize) -> T {
        let removed = self.values.swap_remove(index);
        if let Some(moved) = self.values.get(index) {
            // XX: can used relaxed
            Pt::point(moved).0.store(index, Ordering::Relaxed);
        }
        removed
    }
}