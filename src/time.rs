use std::{sync::{atomic::{AtomicI64, Ordering}, OnceLock}, time::{Duration, Instant}};


pub trait Clock {
    fn now(&self) -> Instant;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemInstant;

impl Clock for SystemInstant {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

pub struct AtomicInstant(AtomicI64);

static ZERO: OnceLock<Instant> = OnceLock::new();

fn instant_to_i64(x: Instant) -> i64 {
    let zero = *ZERO.get_or_init(|| x);
    if x > zero {
        (x - zero).as_nanos().try_into().expect("nano duration overflow")
    } else {
        -i64::try_from((zero - x).as_nanos()).expect("nano duration overflow")
    }
}

fn i64_to_instant(x: i64) -> Instant {
    let zero = *ZERO.get().expect("should've already converted instant to i64");

    match u64::try_from(x) {
        Ok(x) => zero + Duration::from_nanos(x),
        Err(_) => zero - Duration::from_nanos(x.unsigned_abs())
    }
}

impl AtomicInstant {
    pub fn new(x: Instant) -> Self {
        Self(AtomicI64::new(instant_to_i64(x)))
    }

    pub fn load(&self, ordering: Ordering) -> Instant {
        i64_to_instant(self.0.load(ordering))
    }

    pub fn store(&self, x: Instant, ordering: Ordering) {
        self.0.store(instant_to_i64(x), ordering);
    }
}