use crate::lock::UpgradeReadGuard;

mod approx;
pub mod generation;
pub mod touch;
pub mod write;
// pub mod multi;

#[cfg(feature = "rand")]
pub mod random;

#[cfg(feature = "rand")]
mod bag;

mod index;
mod list;

pub use approx::EvictApproximate;