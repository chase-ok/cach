use std::{sync::{atomic::{AtomicU64, Ordering}, OnceLock}, time::{Duration, Instant}};


pub trait Clock {
    fn now(&self) -> Instant;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultClock;

impl Clock for DefaultClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

pub trait TouchedTime {
    fn last_touched(&self) -> Instant;
    fn touch(&self, now: Instant);
}

pub trait WrittenTime {
    fn written_time(&self) -> Instant;
}

pub trait ExpiryTime {
    fn expiry_time(&self) -> Option<Instant>;
}

#[derive(Debug)]
pub struct AtomicInstant(AtomicU64);

fn zero() -> Instant {
    static OFFSET: OnceLock<Instant> = OnceLock::new();
    *OFFSET.get_or_init(Instant::now)
}

fn instant_to_offset(instant: Instant, zero: Instant) -> u64 {
    let duration = instant.duration_since(zero);
    // 2^64 nanos = 584 years, plently of uptime before wrapping!
    duration.as_nanos() as u64
}

fn offset_to_instant(offset: u64, zero: Instant) -> Instant {
    zero + Duration::from_nanos(offset)
}

impl From<Instant> for AtomicInstant {
    fn from(value: Instant) -> Self {
        Self::new(value)
    }
}

impl AtomicInstant {
    pub fn new(instant: Instant) -> Self {
        Self(instant_to_offset(instant, zero()).into())
    }

    pub fn load(&self, order: Ordering) -> Instant {
        offset_to_instant(self.0.load(order), zero())
    }

    pub fn store(&self, value: Instant, order: Ordering) {
        self.0.store(instant_to_offset(value, zero()), order);
    }
    
    pub fn swap(&self, value: Instant, order: Ordering) -> Instant {
        let zero = zero();
        offset_to_instant(self.0.swap(instant_to_offset(value, zero), order), zero)
    }
}