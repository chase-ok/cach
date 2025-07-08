use std::{
    cell::Cell,
    num::{NonZero, Wrapping},
    sync::atomic::{AtomicIsize, AtomicUsize, Ordering},
};

use crossbeam_utils::CachePadded;

const ID_UNSET: usize = 0;

std::thread_local! {
    static ID: Cell<usize> = const { Cell::new(ID_UNSET) };
}

pub(crate) fn id() -> usize {
    let id = ID.get();
    if id != ID_UNSET {
        id
    } else {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(ID_UNSET + 1);

        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        ID.set(id);
        id
    }
}

#[derive(Debug)]
pub(crate) struct ThreadSharded<T> {
    values: Box<[CachePadded<T>]>,
    mask: usize,
}

impl<T: Default> Default for ThreadSharded<T> {
    fn default() -> Self {
        Self::new(Default::default)
    }
}

impl<T> ThreadSharded<T> {
    pub fn new(f: impl FnMut() -> T) -> Self {
        let values = sharded(f);
        Self {
            mask: values.len() - 1,
            values,
        }
    }

    pub fn get(&self) -> &T {
        &self.values[id() & self.mask]
    }

    pub fn get_mut(&mut self) -> &mut T {
        &mut self.values[id() & self.mask]
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> + ExactSizeIterator {
        self.values.iter().map(|v| &**v)
    }
}

#[derive(Debug, Default)]
pub(crate) struct ThreadShardedCounter(ThreadSharded<AtomicIsize>);

impl ThreadShardedCounter {
    pub fn add(&self, value: isize) {
        self.0.get().fetch_add(value, Ordering::Relaxed);
    }

    pub fn increment(&self) {
        self.add(1);
    }

    pub fn decrement(&self) {
        self.add(-1);
    }

    pub fn get(&self) -> isize {
        // XX does this actually work for imbalanced inc/dec by thread?
        self.0
            .iter()
            .map(|c| Wrapping(c.load(Ordering::Relaxed)))
            .sum::<Wrapping<isize>>()
            .0
    }

    pub fn clear(&self) {
        for c in self.0.iter() {
            c.store(0, Ordering::Relaxed);
        }
    }
}

#[derive(Debug)]
pub(crate) struct ThreadRngSharded<T> {
    values: Box<[CachePadded<T>]>,
    mask: usize,
}

impl<T: Default> Default for ThreadRngSharded<T> {
    fn default() -> Self {
        Self::new(Default::default)
    }
}

impl<T> ThreadRngSharded<T> {
    pub fn new(f: impl FnMut() -> T) -> Self {
        let values = sharded(f);
        Self { mask: values.len() - 1, values}
    }

    pub fn get(&self) -> &T {
        &self.values[fastrand::usize(..) & self.mask]
    }

    pub fn get_mut(&mut self) -> &mut T {
        &mut self.values[fastrand::usize(..) & self.mask]
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> + ExactSizeIterator {
        self.values.iter().map(|v| &**v)
    }
}

fn sharded<T>(f: impl FnMut() -> T) -> Box<[CachePadded<T>]> {
    let num_shards = std::thread::available_parallelism()
        .map(NonZero::get)
        .unwrap_or(4)
        .checked_next_power_of_two()
        .unwrap_or(128);
    std::iter::repeat_with(f)
        .map(CachePadded::new)
        .take(num_shards)
        .collect()
}
