use super::index::{Generation, Index, Key};

pub struct List<T> {
    nodes: Vec<Node<T>>,
    len: usize,
    head: Option<Index>,
    tail: Option<Index>,
    next_free: Option<Key>,
}

struct Node<T> {
    state: NodeState<T>,
    gen: Generation,
}

enum NodeState<T> {
    Occupied {
        value: T,
        prev: Option<Index>,
        next: Option<Index>,
    },
    Vacant {
        next_free: Option<Key>,
    },
}

#[doc(hidden)]
impl<T> List<T> {
    pub fn with_capacity(capacity: usize) -> Self {
        assert!(capacity > 0, "capacity must not be empty");
        assert!(capacity <= Index::MAX.into(), "capacity too large");

        Self {
            nodes: Vec::with_capacity(capacity),
            len: 0,
            head: None,
            tail: None,
            next_free: None,
        }
    }

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn capacity(&self) -> usize {
        self.nodes.capacity()
    }

    pub fn push_tail_with_key_and_pop_if_full(&mut self, value: impl FnOnce(Key) -> T) -> (&T, Option<T>) {
        debug_assert!(self.capacity() > 0);
        let removed = if self.len() == self.capacity() {
            self.pop_head()
        } else {
            None
        };
        let pushed = self.push_tail_with_key(value);
        (pushed, removed)
    }

    pub fn push_tail_with_key(&mut self, value: impl FnOnce(Key) -> T) -> &T {
        let tail = self.tail;
        let node_state = |value| NodeState::Occupied {
            value,
            prev: None,
            next: tail,
        };

        let key = match self.next_free {
            None => {
                assert!(self.nodes.len() < Index::MAX.into(), "out of capacity");

                let gen = Generation::initial();
                let key = Key {
                    index: self.nodes.len().into(),
                    gen,
                };
                let state = node_state(value(key));
                self.nodes.push(Node { state, gen });
                key
            }

            Some(key) => {
                let node = &mut self.nodes[key.index.into_usize()];
                assert_eq!(key.gen, node.gen);

                self.next_free = match &node.state {
                    NodeState::Vacant { next_free } => *next_free,
                    _ => unreachable!(),
                };

                node.gen.increment_mut();
                node.state = node_state(value(key));
                key
            }
        };

        let NodeState::Occupied { value, .. } = &self.nodes[key.index.into_usize()].state else {
            unreachable!()
        };
        value
    }

    pub fn pop_head(&mut self) -> Option<T> {
        let index = self.head?;
        self.remove(Key {
            index,
            gen: self.nodes[index.into_usize()].gen,
        })
    }

    pub fn remove(&mut self, key: Key) -> Option<T> {
        let node = self.nodes.get_mut(key.index.into_usize())?;
        if node.gen != key.gen {
            return None;
        }

        let NodeState::Occupied { value, prev, next } = std::mem::replace(
            &mut node.state,
            NodeState::Vacant {
                next_free: self.next_free,
            },
        ) else {
            unreachable!()
        };

        node.gen.increment_mut();
        self.next_free = Some(Key {
            index: key.index,
            gen: node.gen,
        });

        self.prune_link(prev, next);

        Some(value)
    }

    fn prune_link(&mut self, prev: Option<Index>, next: Option<Index>) {
        if let Some(next) = next {
            match &mut self.nodes[next.into_usize()].state {
                NodeState::Occupied {
                    prev: next_prev, ..
                } => *next_prev = prev,
                _ => unreachable!(),
            }
        } else {
            self.head = prev;
        }

        if let Some(prev) = prev {
            match &mut self.nodes[prev.into_usize()].state {
                NodeState::Occupied {
                    next: prev_next, ..
                } => *prev_next = next,
                _ => unreachable!(),
            }
        } else {
            self.tail = next;
        }
    }

    pub fn move_to_tail(&mut self, key: Key) {
        let Some(node) = self
            .nodes
            .get_mut(key.index.into_usize())
            .filter(|n| n.gen == key.gen)
        else {
            return;
        };

        let current_tail = self.tail.unwrap();
        if current_tail == key.index {
            return;
        }

        let NodeState::Occupied { prev, next, .. } = &mut node.state else {
            unreachable!()
        };
        let prev = std::mem::replace(prev, None);
        let next = std::mem::replace(next, Some(current_tail));
        self.prune_link(prev, next);

        match &mut self.nodes[current_tail.into_usize()].state {
            NodeState::Occupied { prev, .. } => {
                *prev = Some(key.index);
            }
            _ => unreachable!(),
        }
    }

    pub fn get(&self, key: Key) -> Option<&T> {
        self.nodes
            .get(key.index.into_usize())
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
