use std::{
    hint::spin_loop,
    marker::PhantomData,
    mem::ManuallyDrop,
    ops::Deref,
    pin::Pin,
    process::abort,
    ptr::NonNull,
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

use stable_deref_trait::{CloneStableDeref, StableDeref};

#[repr(transparent)]
pub struct Arc<T: ?Sized> {
    ptr: NonNull<ArcInner<T>>,
    _marker: PhantomData<T>,
}

#[repr(align(8))] // XX at least 8!
struct ArcInner<T: ?Sized> {
    count: AtomicUsize,
    value: T,
}

pub struct AtomicArc<T>(AtomicPtr<ArcInner<T>>);

pub struct AtomicArcGuard<'a, T> {
    atomic: &'a AtomicPtr<ArcInner<T>>,
    // XX does const vs mut matter?
    tagged_ptr: *mut ArcInner<T>,
    ptr: Option<NonNull<ArcInner<T>>>,
}

impl<T> Arc<T> {
    #[must_use]
    pub fn new(value: T) -> Self {
        assert!(std::mem::align_of::<ArcInner<T>>() >= MIN_ALIGN);

        let ptr = Box::into_raw(Box::new(ArcInner {
            count: 1.into(),
            value,
        }));
        // Safety: Box::into_raw guaranteed not to be null
        let ptr = unsafe { NonNull::new_unchecked(ptr) };

        Self {
            ptr,
            _marker: PhantomData,
        }
    }

    /// Constructs a new `Pin<Arc<T>>`. If `T` does not implement `Unpin`, then
    /// `data` will be pinned in memory and unable to be moved.
    #[must_use]
    pub fn pin(data: T) -> Pin<Arc<T>> {
        unsafe { Pin::new_unchecked(Self::new(data)) }
    }

    fn from_ptr(ptr: *mut ArcInner<T>) -> Option<Self> {
        debug_assert_eq!(ptr.addr() & TAG_MASK, 0);
        NonNull::new(ptr).map(|ptr| Self {
            ptr,
            _marker: PhantomData,
        })
    }
}

impl<T: ?Sized> Arc<T> {
    fn inner(&self) -> &ArcInner<T> {
        // This unsafety is ok because while this arc is alive we're guaranteed
        // that the inner pointer is valid. Furthermore, we know that the
        // `ArcInner` structure itself is `Sync` because the inner data is
        // `Sync` as well, so we're ok loaning out an immutable pointer to these
        // contents.
        unsafe { self.ptr.as_ref() }
    }

    #[inline(never)]
    unsafe fn drop_slow(&mut self) {
        let _ = unsafe { Box::from_raw(self.ptr.as_ptr()) };
    }
}

// XX safety
unsafe impl<T: ?Sized + Send + Sync> Send for Arc<T> {}
unsafe impl<T: ?Sized + Send + Sync> Sync for Arc<T> {}

unsafe impl<T: ?Sized + Sync + Send> Send for ArcInner<T> {}
unsafe impl<T: ?Sized + Sync + Send> Sync for ArcInner<T> {}

// XX impl for Guards?

impl<T: ?Sized> Clone for Arc<T> {
    #[inline]
    fn clone(&self) -> Self {
        // Using a relaxed ordering is alright here, as knowledge of the
        // original reference prevents other threads from erroneously deleting
        // the object.
        //
        // As explained in the [Boost documentation][1], Increasing the
        // reference counter can always be done with memory_order_relaxed: New
        // references to an object can only be formed from an existing
        // reference, and passing an existing reference from one thread to
        // another must already provide any required synchronization.
        //
        // [1]: (www.boost.org/doc/libs/1_55_0/doc/html/atomic/usage_examples.html)
        let old_size = self.inner().count.fetch_add(1, Ordering::Relaxed);

        // However we need to guard against massive refcounts in case someone
        // is `mem::forget`ing Arcs. If we don't do this the count can overflow
        // and users will use-after free. We racily saturate to `isize::MAX` on
        // the assumption that there aren't ~2 billion threads incrementing
        // the reference count at once. This branch will never be taken in
        // any realistic program.
        //
        // We abort because such a program is incredibly degenerate, and we
        // don't care to support it.
        if old_size > (isize::MAX) as usize {
            abort();
        }

        Self {
            ptr: self.ptr,
            _marker: PhantomData,
        }
    }
}

impl<T: ?Sized> Deref for Arc<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner().value
    }
}

unsafe impl<T: ?Sized> StableDeref for Arc<T> {}
unsafe impl<T: ?Sized> CloneStableDeref for Arc<T> {}

impl<T: ?Sized> Drop for Arc<T> {
    #[inline]
    fn drop(&mut self) {
        // Because `fetch_sub` is already atomic, we do not need to synchronize
        // with other threads unless we are going to delete the object.
        if self.inner().count.fetch_sub(1, Ordering::Release) != 1 {
            return;
        }

        // This fence is needed to prevent reordering of use of the data and
        // deletion of the data. Because it is marked `Release`, the decreasing
        // of the reference count synchronizes with this `Acquire` fence. This
        // means that use of the data happens before decreasing the reference
        // count, which happens before this fence, which happens before the
        // deletion of the data.
        //
        // As explained in the [Boost documentation][1],
        //
        // > It is important to enforce any possible access to the object in one
        // > thread (through an existing reference) to *happen before* deleting
        // > the object in a different thread. This is achieved by a "release"
        // > operation after dropping a reference (any access to the object
        // > through this reference must obviously happened before), and an
        // > "acquire" operation before deleting the object.
        //
        // In particular, while the contents of an Arc are usually immutable, it's
        // possible to have interior writes to something like a Mutex<T>. Since a
        // Mutex is not acquired when it is deleted, we can't rely on its
        // synchronization logic to make writes in thread A visible to a destructor
        // running in thread B.
        //
        // Also note that the Acquire fence here could probably be replaced with an
        // Acquire load, which could improve performance in highly-contended
        // situations. See [2].
        //
        // [1]: (www.boost.org/doc/libs/1_55_0/doc/html/atomic/usage_examples.html)
        // [2]: (https://github.com/rust-lang/rust/pull/41714)
        self.inner().count.load(Ordering::Acquire);

        unsafe {
            self.drop_slow();
        }
    }
}

const MIN_ALIGN: usize = 8;
const TAG_MASK: usize = MIN_ALIGN - 1;
const WRITER_WAITING_BIT: usize = 0b1;
const READER_MASK: usize = TAG_MASK & !WRITER_WAITING_BIT;
const ONE_READER: usize = WRITER_WAITING_BIT << 1;
const MAX_READERS: usize = READER_MASK / ONE_READER;

impl<T> AtomicArc<T> {
    pub const fn none() -> Self {
        Self(AtomicPtr::new(std::ptr::null_mut()))
    }

    pub fn some(arc: Arc<T>) -> Self {
        let arc = ManuallyDrop::new(arc);
        Self(AtomicPtr::new(arc.ptr.as_ptr()))
    }

    pub fn new(arc: Option<Arc<T>>) -> Self {
        match arc {
            None => Self::none(),
            Some(arc) => Self::some(arc),
        }
    }

    #[inline]
    pub fn load(&self) -> AtomicArcGuard<'_, T> {
        let mut tagged_ptr = self.0.load(Ordering::Relaxed);
        let mut backoff = Backoff::default();

        loop {
            let (tag, ptr) = split_tagged_ptr(tagged_ptr);

            if tag & WRITER_WAITING_BIT != 0 || (tag & READER_MASK) / ONE_READER >= MAX_READERS {
                backoff.spin();
                tagged_ptr = self.0.load(Ordering::Acquire);
                continue;
            }

            let new_tag = tag + ONE_READER;
            debug_assert_eq!(new_tag & !TAG_MASK, 0);

            let new_tagged_ptr = ptr.map_addr(|a| a | new_tag);
            match self.0.compare_exchange_weak(
                tagged_ptr,
                new_tagged_ptr,
                Ordering::Release,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return AtomicArcGuard {
                        atomic: &self.0,
                        tagged_ptr: new_tagged_ptr,
                        ptr: NonNull::new(ptr),
                    };
                }
                Err(p) => {
                    tagged_ptr = p;
                    backoff.reset();
                }
            }
        }
    }

    // #[inline]
    // pub fn store(&self, arc: Option<Arc<T>>) -> Option<Arc<T>> {
    //     let Ok(prev) = self.store_if(arc, |_| true) else {
    //         unreachable!("always overwrites")
    //     };
    //     prev
    // }

    // #[inline]
    // pub fn store_if_eq(
    //     &self,
    //     current: Option<&Arc<T>>,
    //     new: Option<Arc<T>>,
    // ) -> Result<Option<Arc<T>>, Option<Arc<T>>> {
    //     let current = current
    //         .map(|c| c.ptr.as_ptr())
    //         .unwrap_or(std::ptr::null_mut());
    //     self.store_if(new, |a| a == current)
    // }

    // fn store_if(
    //     &self,
    //     arc: Option<Arc<T>>,
    //     mut f: impl FnMut(*mut ArcInner<T>) -> bool,
    // ) -> Result<Option<Arc<T>>, Option<Arc<T>>> {
    //     let arc = ManuallyDrop::new(arc);
    //     let new_ptr = arc
    //         .as_ref()
    //         .map(|a| a.ptr.as_ptr())
    //         .unwrap_or(std::ptr::null_mut());

    //     let mut tagged_ptr = self.0.load(Ordering::Relaxed);
    //     let mut backoff = Backoff::default();

    //     let no_reader_ptr = loop {
    //         let (tag, ptr) = split_tagged_ptr(tagged_ptr);

    //         // XX this is wrong, might be stale pointer
    //         if !f(ptr) {
    //             return Err(ManuallyDrop::into_inner(arc));
    //         }

    //         if tag == 0 {
    //             match self.0.compare_exchange_weak(
    //                 tagged_ptr,
    //                 new_ptr,
    //                 Ordering::Release,
    //                 Ordering::Acquire,
    //             ) {
    //                 Ok(_) => return Ok(Arc::from_ptr(ptr)),
    //                 Err(p) => {
    //                     tagged_ptr = p;
    //                     backoff.reset();
    //                 }
    //             }
    //         } else if tag & WRITER_WAITING_BIT != 0 {
    //             backoff.spin();
    //             tagged_ptr = self.0.load(Ordering::Acquire);
    //         } else {
    //             let new_tag = tag | WRITER_WAITING_BIT;
    //             debug_assert_eq!(new_tag & !TAG_MASK, 0);

    //             let new_tagged_ptr = ptr.map_addr(|a| a | new_tag);
    //             backoff.reset();
    //             match self.0.compare_exchange_weak(
    //                 tagged_ptr,
    //                 new_tagged_ptr,
    //                 Ordering::Release,
    //                 Ordering::Acquire,
    //             ) {
    //                 Ok(_) => break tagged_ptr.map_addr(|a| a & !READER_MASK),
    //                 Err(p) => tagged_ptr = p,
    //             }
    //         }
    //     };

    //     while self
    //         .0
    //         .compare_exchange(no_reader_ptr, new_ptr, Ordering::Release, Ordering::Acquire)
    //         .is_err()
    //     {
    //         backoff.spin();
    //     }

    //     Ok(Arc::from_ptr(no_reader_ptr.map_addr(|a| a & !TAG_MASK)))
    // }
}

impl<'a, T> AtomicArcGuard<'a, T> {
    #[inline]
    pub fn arc(&self) -> Option<&Arc<T>> {
        self.ptr
            .as_ref()
            // Safe because of repr(transparent) on Arc with single non null field
            .map(|ptr| unsafe { std::mem::transmute::<&NonNull<ArcInner<T>>, &Arc<T>>(ptr) })
    }

    #[inline]
    pub fn value(&self) -> Option<&T> {
        self.arc().map(|a| &**a)
    }

    pub fn try_store(self, arc: Option<Arc<T>>) -> Result<Option<Arc<T>>, Option<Arc<T>>> {
        let this = ManuallyDrop::new(self);
        let arc = ManuallyDrop::new(arc);

        let new_ptr = arc
            .as_ref()
            .map(|a| a.ptr.as_ptr())
            .unwrap_or(std::ptr::null_mut());

        let mut tagged_ptr = this.tagged_ptr;
        let mut backoff = Backoff::default();

        let one_reader_ptr = loop {
            let (tag, ptr) = split_tagged_ptr(tagged_ptr);

            if tag == ONE_READER {
                match this.atomic.compare_exchange_weak(
                    tagged_ptr,
                    new_ptr,
                    Ordering::Release,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return Ok(Arc::from_ptr(ptr)),
                    Err(p) => {
                        tagged_ptr = p;
                        backoff.reset();
                    }
                }
            } else if tag & WRITER_WAITING_BIT != 0 {
                let _ = ManuallyDrop::into_inner(this); // still clear reader flag
                return Err(ManuallyDrop::into_inner(arc));
            } else {
                let new_tag = tag | WRITER_WAITING_BIT;
                debug_assert_eq!(new_tag & !TAG_MASK, 0);

                let new_tagged_ptr = ptr.map_addr(|a| a | new_tag);
                backoff.reset();
                match this.atomic.compare_exchange_weak(
                    tagged_ptr,
                    new_tagged_ptr,
                    Ordering::Release,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break tagged_ptr.map_addr(|a| a & !READER_MASK | ONE_READER),
                    Err(p) => tagged_ptr = p,
                }
            }
        };

        while this
            .atomic
            .compare_exchange(
                one_reader_ptr,
                new_ptr,
                Ordering::Release,
                Ordering::Acquire,
            )
            .is_err()
        {
            backoff.spin();
        }

        Ok(Arc::from_ptr(one_reader_ptr.map_addr(|a| a & !TAG_MASK)))
    }
}

impl<T> Drop for AtomicArcGuard<'_, T> {
    #[inline]
    fn drop(&mut self) {
        let mut tagged_ptr = self.tagged_ptr;
        let mut backoff = Backoff::default();

        loop {
            backoff.spin();
            let (tag, ptr) = split_tagged_ptr(tagged_ptr);

            debug_assert!((tag & READER_MASK) / ONE_READER >= 1);
            let new_tag = tag - ONE_READER;
            debug_assert_eq!(new_tag & !TAG_MASK, 0);

            let new_tagged_ptr = ptr.map_addr(|a| a | new_tag);
            match self.atomic.compare_exchange_weak(
                tagged_ptr,
                new_tagged_ptr,
                Ordering::Release,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(p) => tagged_ptr = p,
            }
        }
    }
}

impl<T> Drop for AtomicArc<T> {
    fn drop(&mut self) {
        let ptr = *self.0.get_mut();
        assert_eq!(ptr.addr() & TAG_MASK, 0);
        let _ = Arc::from_ptr(ptr);
    }
}

#[inline]
fn split_tagged_ptr<T>(tagged_ptr: *mut ArcInner<T>) -> (usize, *mut ArcInner<T>) {
    let tag = tagged_ptr.addr() & TAG_MASK;
    let ptr = tagged_ptr.map_addr(|a| a & !TAG_MASK);
    (tag, ptr)
}

// XX replace with cross beam?
#[derive(Default)]
struct Backoff(u8);

impl Backoff {
    fn spin(&mut self) {
        if self.0 < 10 {
            for _ in 0..self.0 {
                spin_loop();
            }
            self.0 += 1;
        } else {
            std::thread::yield_now();
        }
    }

    fn reset(&mut self) {
        self.0 = 0;
    }
}
