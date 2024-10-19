use std::{
    num::NonZero,
    sync::atomic::{AtomicU32, Ordering},
};

use crossbeam_utils::Backoff;

pub struct IndexList<T> {
    nodes: Vec<Node<T>>,
    head: AtomicIndex,
    tail: AtomicIndex,
    len: usize,
    next_free: Option<Key>,
}

type IndexRepr = u32;
type AtomicIndexRepr = AtomicU32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Index(IndexRepr);

const UNSET: IndexRepr = 1 << (IndexRepr::BITS - 1);
const DELETED: IndexRepr = 1 << (IndexRepr::BITS - 2);
const INDEX_MASK: IndexRepr = !DELETED & !UNSET;

impl Index {
    fn from_repr(x: IndexRepr) -> Option<Self> {
        if x & UNSET > 0 {
            None
        } else {
            Some(Self(x))
        }
    }

    fn to_repr(this: Option<Self>) -> IndexRepr {
        match this {
            Some(Self(x)) => {
                debug_assert_eq!(x & UNSET, 0);
                x
            }
            None => UNSET,
        }
    }

    fn new(mut offset: IndexRepr, deleted: bool) -> Self {
        debug_assert_eq!(offset & UNSET, 0);
        debug_assert_eq!(offset & DELETED, 0);
        if deleted {
            offset |= DELETED;
        }
        Self(offset)
    }

    fn offset(self) -> IndexRepr {
        self.0 & INDEX_MASK
    }

    fn is_deleted(self) -> bool {
        self.0 & DELETED > 0
    }

    fn delete(self) -> Self {
        Self(self.0 | DELETED)
    }
}

#[derive(Debug)]
struct AtomicIndex(AtomicIndexRepr);

impl AtomicIndex {
    const fn unset() -> Self {
        Self(AtomicIndexRepr::new(UNSET))
    }
    
    fn new(index: Option<Index>) -> Self {
        Self(AtomicIndexRepr::new(Index::to_repr(index)))
    }

    fn load(&self, order: Ordering) -> Option<Index> {
        Index::from_repr(self.0.load(order))
    }

    fn store(&self, index: Option<Index>, order: Ordering) {
        self.0.store(Index::to_repr(index), order);
    }

    fn get(&mut self) -> Option<Index> {
        Index::from_repr(*self.0.get_mut())
    }
    
    fn set(&mut self, index: Option<Index>) {
        *self.0.get_mut() = Index::to_repr(index);
    }
}

type Gen = NonZero<u32>;
const START_GEN: Gen = Gen::MIN;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    offset: IndexRepr,
    gen: Gen,
}

struct Node<T> {
    state: NodeState<T>,
    gen: Gen,
}

enum NodeState<T> {
    Occupied {
        value: T,
        prev: AtomicIndex,
        next: AtomicIndex,
    },
    Vacant {
        next_free: Option<Key>,
    },
}

impl<T> IndexList<T> {
    pub fn with_capacity(capacity: usize) -> Self {
        assert!(capacity & (!INDEX_MASK) as usize == 0, "capacity overflow");

        Self {
            nodes: Vec::with_capacity(capacity),
            head: AtomicIndex::unset(),
            tail: AtomicIndex::unset(),
            len: 0,
            next_free: None,
        }
    }

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn capacity(&self) -> usize {
        self.nodes.capacity()
    }

    pub fn push_tail_with_key(&mut self, value: impl FnOnce(Key) -> T) -> (Key, &T) {
        let tail = self.tail.get();
        let node_state = |value| NodeState::Occupied {
            value,
            prev: AtomicIndex::unset(),
            next: AtomicIndex::new(tail),
        };

        let key = match self.next_free {
            None => {
                let offset = self.nodes.len().try_into().expect("out of capacity");
                let gen = START_GEN;
                let key = Key { offset, gen };
                let state = node_state(value(key));
                self.nodes.push(Node { state, gen });
                key
            }

            Some(key) => {
                let node = &mut self.nodes[key.offset as usize];
                assert_eq!(key.gen, node.gen);

                self.next_free = match &node.state {
                    NodeState::Vacant { next_free } => *next_free,
                    _ => unreachable!(),
                };

                node.gen = node.gen.checked_add(1).unwrap_or(START_GEN);
                node.state = node_state(value(key));
                key
            }
        };

        let NodeState::Occupied { value, .. } = &self.nodes[key.offset as usize].state else {
            unreachable!()
        };
        (key, value)
    }

    pub fn push_tail(&mut self, value: T) -> (Key, &T) {
        self.push_tail_with_key(|_key| value)
    }

    pub fn pop_head(&mut self) -> Option<T> {
        let index = self.head.get()?;
        debug_assert!(!index.is_deleted());

        self.remove(Key {
            offset: index.offset(),
            gen: self.nodes[index.offset() as usize].gen,
        })
    }

    pub fn remove(&mut self, key: Key) -> Option<T> {
        let node = &mut self.nodes[key.offset as usize];
        assert_eq!(node.gen, key.gen);

        let NodeState::Occupied { value, mut prev, mut next } = std::mem::replace(
            &mut node.state,
            NodeState::Vacant {
                next_free: self.next_free,
            },
        ) else {
            unreachable!()
        };

        let gen = node.gen.checked_add(1).unwrap_or(START_GEN);
        node.gen = gen;
        self.next_free = Some(Key {
            offset: key.offset,
            gen,
        });

        self.prune_link(prev.get(), next.get());

        Some(value)
    }

    fn prune_link(&mut self, prev: Option<Index>, next: Option<Index>) {
        if let Some(next) = next {
            debug_assert!(!next.is_deleted());
            match &mut self.nodes[next.offset() as usize].state {
                NodeState::Occupied {
                    prev: next_prev, ..
                } => next_prev.set(prev),
                _ => unreachable!(),
            }
        } else {
            self.head.set(prev);
        }

        if let Some(prev) = prev {
            debug_assert!(!prev.is_deleted());
            match &mut self.nodes[prev.offset() as usize].state {
                NodeState::Occupied {
                    next: prev_next, ..
                } => prev_next.set(next),
                _ => unreachable!(),
            }
        } else {
            self.tail.set(next);
        }
    }

    pub fn move_to_tail_locked(&mut self, key: Key) {
        let node = &mut self.nodes[key.offset as usize];
        assert_eq!(node.gen, key.gen);

        let current_tail = self.tail.get().unwrap();
        debug_assert!(!current_tail.is_deleted());
        if current_tail.offset() == key.offset {
            return;
        }

        let NodeState::Occupied { prev, next, .. } = &mut node.state else {
            unreachable!()
        };
        let mut prev = std::mem::replace(prev, AtomicIndex::unset());
        let mut next = std::mem::replace(next, AtomicIndex::new(Some(current_tail)));
        self.prune_link(prev.get(), next.get());

        match &mut self.nodes[current_tail.offset() as usize].state {
            NodeState::Occupied { prev, .. } => {
                prev.set(Some(Index::new(key.offset, false)));
            }
            _ => unreachable!(),
        }
    }

    pub fn move_to_tail(&self, key: Key) {
        let node = &self.nodes[key.offset as usize];
        assert_eq!(node.gen, key.gen);
        
        let backoff = Backoff::new();
        let mut current_tail = self.tail.load(Ordering::Relaxed).unwrap(); 
        loop {
            if !current_tail.is_deleted() {
                if current_tail.offset() == key.offset {
                    return;
                }
                
                self.tail.compare_exchange_weak(Some(current_tail), Some(current_tail.delete()), Ordering::AcqRel, Ordering::Acquire)

            }

            let NodeState::Occupied { prev, next, .. } = &node.state else {
                unreachable!()
            };

            backoff.spin();
        }
    }

    pub fn get(&self, key: Key) -> Option<&T> {
        self.nodes
            .get(key.offset as usize)
            .filter(|node| node.gen == key.gen)
            .and_then(|node| match &node.state {
                NodeState::Occupied { value, .. } => Some(value),
                NodeState::Vacant { .. } => None,
            })
    }

    pub fn drain(&mut self) -> impl Iterator<Item = T> + '_ {
        std::iter::from_fn(|| self.pop_head())
    }
}
