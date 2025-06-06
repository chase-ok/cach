use smallvec::SmallVec;

use super::{
    Insert, InsertShared, Lock, PurgeShared, ReadExclusive, ReadShared, RemoveShared, Strategy,
    StrategyPointer,
};

const BUF_CAP: usize = 16;

pub struct BufferedInsertsStrategy<P, S> {
    buffer: scc::Bag<(P, Action), BUF_CAP>,
    cap: usize,
    inner: S,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Action {
    Insert,
    Remove,
}

impl<P, S> BufferedInsertsStrategy<P, S>
where
    P: StrategyPointer<StrategyValue = S::Value>,
    S: Strategy<P>,
{
    fn drain(&mut self) {
        let mut vec = SmallVec::<[(P, Action); BUF_CAP]>::with_capacity(self.buffer.len());
        self.buffer.pop_all(&mut vec, |vec, a| {
            vec.push(a);
            vec
        });

        fn as_ptr<P: StrategyPointer>(p: &P) -> *const P::Target {
            &**p as *const P::Target
        }

        // XX this relies on clone stable deref!
        vec.sort_unstable_by_key(|(p, a)| (as_ptr(p), *a));

        while let Some((pointer, action)) = vec.pop() {
            match action {
                Action::Insert => {
                    self.inner.complete_insert(&pointer);
                }
                Action::Remove => {
                    if vec.last().is_some_and(|(p, a)| {
                        *a == Action::Insert && std::ptr::addr_eq(as_ptr(p), as_ptr(&pointer))
                    }) {
                        vec.pop();
                        // dedup insert and remove
                    } else {
                        self.inner.remove(&pointer);
                    }
                }
            }
            debug_assert!(
                vec.last()
                    .is_none_or(|(p, _)| !std::ptr::addr_eq(as_ptr(p), as_ptr(&pointer)))
            );
        }
    }
}

impl<P, S> Strategy<P> for BufferedInsertsStrategy<P, S>
where
    P: StrategyPointer<StrategyValue = S::Value>,
    S: Strategy<P>,
{
    type Value = S::Value;

    const READ_LOCK: Lock = S::READ_LOCK;

    const REMOVE_LOCK: Lock = Lock::Shared;
    const INSERT_LOCK: Lock = Lock::Shared;
    const PURGE_LOCK: Lock = Lock::Exclusive;

    fn create_insert_shared_value(&self, target: &P::Target) -> Self::Value {
        self.inner.create_insert_shared_value(target)
    }

    fn create_insert_value(&mut self, target: &P::Target) -> Self::Value {
        self.inner.create_insert_value(target)
    }

    fn read_shared(&self, pointer: &P) -> ReadShared {
        self.inner.read_shared(pointer)
    }

    fn read(&mut self, pointer: &P) -> ReadExclusive {
        self.inner.read(pointer)
    }

    fn start_remove_shared(&self, _pointer: &P) -> RemoveShared {
        if self.buffer.len() < self.cap {
            RemoveShared::Allow
        } else {
            RemoveShared::RequireExclusive
        }
    }

    fn complete_remove_shared(&self, pointer: &P) {
        self.buffer.push((pointer.clone(), Action::Remove));
    }

    fn remove(&mut self, pointer: &P) {
        self.drain();
        self.inner.remove(pointer);
    }

    fn start_insert_shared(&self, _target: &P::Target) -> InsertShared {
        if self.buffer.len() < self.cap {
            InsertShared::Allow
        } else {
            InsertShared::RequireExclusive
        }
    }

    fn fail_insert_shared(&self, _target: &P::Target, _value: Self::Value) {}

    fn complete_insert_shared(&self, pointer: &P) {
        self.buffer.push((pointer.clone(), Action::Insert));
    }

    fn start_insert(&mut self, target: &P::Target) -> Insert {
        self.drain();
        self.inner.start_insert(target)
    }

    fn complete_insert(&mut self, pointer: &P) {
        self.inner.complete_insert(pointer);
    }

    fn start_purge_shared(&self) -> PurgeShared<impl Iterator<Item = P>> {
        PurgeShared::RequireExclusive::<std::iter::Empty<P>>
    }

    fn complete_purge_shared(&self, _pointer: &P) {}

    fn purge(&mut self) -> impl Iterator<Item = P> {
        self.drain();
        self.inner.purge()
    }
}
