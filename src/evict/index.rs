
pub struct IndexList<T> {
    nodes: Vec<Node<T>>,
    head: Index,
    tail: Index,
    len: usize,
}

type Index = u32;
type Generation = u32;

const UNSET: u32 = u32::MAX;

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
        prev: Index,
        next: Index,
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
            head: 0,
            tail: 0,
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

    pub fn move_to_tail(&mut self, key: Key) {

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