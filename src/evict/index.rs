use crossbeam_utils::Backoff;
#[cfg(loom)]
use loom::sync;

#[cfg(not(loom))]
use std::sync;

use sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub struct IndexList<T> {
    nodes: Vec<Node<T>>,
    head: AtomicIndex,
    tail: AtomicIndex,
    len: usize,
}

type Index = u32;
type Generation = u32;
type AtomicIndex = AtomicU32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Link(u64);

const DEL_MASK: u64 = 1 << 63;
const PREV_MASK: u64 = u64::MAX >> 32;
const NEXT_MASK: u64 = !PREV_MASK & !DEL_MASK;
const UNSET: u32 = u32::MAX >> 1;

impl Link {
    fn deleted(self) -> bool {
        self.0 & DEL_MASK > 0
    }

    fn next(self) -> u32 {
        ((self.0 & NEXT_MASK) >> 32) as u32
    }

    fn prev(self) -> u32 {
        (self.0 & PREV_MASK) as u32
    }

    fn new(prev: u32, next: u32, deleted: bool) -> Self {
        let mut combined = (prev as u64) & ((next << 32) as u64);
        debug_assert_eq!(combined & DEL_MASK, 0);
        if deleted {
            combined &= DEL_MASK;
        }
        Self(combined)
    }

    fn mark_deleted(self) -> Self {
        Self(self.0 & DEL_MASK)
    }
    
    fn set_prev(self, prev: u32) -> Self {
        Self((self.0 & !PREV_MASK) & (prev as u64))
    }
    
    fn set_next(self, next: u32) -> Self {
        Self((self.0 & !NEXT_MASK) & ((next as u64) << 32))
    }
}

struct AtomicLink(AtomicU64);

impl AtomicLink {
    fn load(&self, order: Ordering) -> Link {
        Link(self.0.load(order))
    }

    fn compare_exchange_weak(
        &self,
        current: Link,
        new: Link,
        success: Ordering,
        failure: Ordering,
    ) -> Result<Link, Link> {
        self.0
            .compare_exchange_weak(current.0, new.0, success, failure)
            .map(Link)
            .map_err(Link)
    }

    fn try_delete(&self) -> Result<Link, Link> {
        let backoff = Backoff::new();
        let mut current = self.load(Ordering::Acquire);
        loop {
            if current.deleted() {
                return Err(current);
            }

            match self.compare_exchange_weak(
                current,
                current.mark_deleted(),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(new) => return Ok(new),
                Err(new) => current = new,
            }
            backoff.spin();
        }
    }

    // fn try_mark_deleted(&self, index: u32, success: Ordering, failure: Ordering) -> Result<(),  {
    //     self.0.compare_exchange_weak(current, new, success, failure)
    // }

    // fn store(&self, mut index: u32, del: bool, order: Ordering) {
    //     debug_assert_eq!(index & DEL_MASK, 0);
    //     if del {
    //         index &= DEL_MASK;
    //     }
    //     self.0.store(index, order);
    // }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key {
    index: Index,
    gen: Generation,
}

struct Node<T> {
    state: NodeState<T>,
    gen: Generation,
}

enum NodeState<T> {
    Occupied {
        value: T,
        link: AtomicLink,
    },
    Vacant {
        next_free: Index,
    },
}

impl<T> IndexList<T> {
    pub fn with_capacity(capacity: usize) -> Self {
        // assert!(capacity < UNSET as usize);

        Self {
            nodes: Vec::with_capacity(capacity),
            head: 0.into(),
            tail: 0.into(),
            len: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn capacity(&self) -> usize {
        self.nodes.capacity()
    }

    pub fn insert_head_with_key(&mut self, value: impl FnOnce(Key) -> T) -> (Key, &T) {
        todo!()
    }

    pub fn insert_tail_with_key(&mut self, value: impl FnOnce(Key) -> T) -> (Key, &T) {
        todo!()
    }

    pub fn insert_head(&mut self, value: T) -> (Key, &T) {
        self.insert_head_with_key(|_key| value)
    }

    pub fn insert_tail(&mut self, value: T) -> (Key, &T) {
        self.insert_tail_with_key(|_key| value)
    }

    pub fn remove(&mut self, index: Key) -> Option<T> {
        todo!()
    }

    pub fn head_key(&self) -> Option<Key> {
        todo!()
    }

    pub fn tail_key(&self) -> Option<Key> {
        todo!()
    }

    pub fn move_to_tail(&self, key: Key) {
        let node = &self.nodes[key.index as usize];
        debug_assert_eq!(key.gen, node.gen);

        let NodeState::Occupied { link, .. } = &node.state else {
            debug_assert!(false);
            return;
        };

        let Ok(deleted) = link.try_delete() else {
            // Someone else is deleting it already to move to tail
            return;
        };

        let mut prev = deleted.prev();
        'found_prev: while prev != UNSET {
            let NodeState::Occupied { link: prev_link, .. } = &self.nodes[prev as usize].state else {
                unreachable!()
            };

            let backoff = Backoff::new();
            let mut current_prev_link = prev_link.load(Ordering::Acquire);
            loop {
                if current_prev_link.deleted() {
                    prev = current_prev_link.prev();
                    break;
                } else {
                    match prev_link.compare_exchange_weak(current_prev_link, current_prev_link.set_next(deleted.next()), Ordering::AcqRel, Ordering::Acquire) {
                        Ok(_) => break 'found_prev,
                        Err(new) => current_prev_link = new,
                    }
                }
                backoff.spin();
            }
        }

        let mut next = deleted.next();
        while next != UNSET {
            let NodeState::Occupied { link: next_link, .. } = &self.nodes[next as usize].state else {
                unreachable!()
            };

            let backoff = Backoff::new();
            let mut current_next_link = next_link.load(Ordering::Acquire);
            loop {
                if current_next_link.deleted() {
                    next = current_next_link.prev();
                    break;
                } else {
                    match next_link.compare_exchange_weak(current_next_link, current_next_link.set_prev(prev), Ordering::AcqRel, Ordering::Acquire) {
                        Ok(_) => break,
                        Err(new) => current_prev_link = new,
                    }
                }
                backoff.spin();
            }
        }
    }

    pub fn get(&self, key: Key) -> Option<&T> {
        self.nodes
            .get(key.index as usize)
            .filter(|node| node.gen == key.gen)
            .and_then(|node| match &node.state {
                NodeState::Occupied { value, .. } => Some(value),
                NodeState::Vacant { .. } => None,
            })
    }
}

fn split(next_and_prev: u64) -> (u32, u32) {
    (
        ((next_and_prev & NEXT_MASK) >> 32) as u32,
        (next_and_prev & PREV_MASK) as u32,
    )
}

fn unsplit(next: u32, prev: u32) -> u64 {
    ((next as u64) << 32) & (prev as u64)
}
