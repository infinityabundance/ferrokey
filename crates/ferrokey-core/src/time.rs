//! The deterministic time representation for the keyboard engine.
//!
//! The state machine and repeat engine take an explicit monotonic moment
//! (`Moment`), a plain `u64` millisecond tick since an arbitrary epoch. This
//! keeps the core fully deterministic AND model-checkable (Kani cannot model
//! `std::time::Instant`, which is backed by a `clock_gettime` foreign call);
//! the UI converts its `Instant` at the boundary with
//! [`Moment::from_elapsed`]. The tick values are only ever compared and
//! offset by `Duration`s, so the representation is exact for the semantics
//! (tap 400 ms, double-tap 500 ms, repeat delay/cadence).

use std::ops::{Add, Sub};
use std::time::{Duration, Instant};

/// A monotonic time point in milliseconds since an arbitrary epoch.
///
/// `u64` milliseconds: the observable time horizon is ~584 million years,
/// and the state machine only ever compares ticks and adds `Duration`s, so
/// wrapping is impossible for real usage (and the model checker treats the
/// arithmetic exactly).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Moment(u64);

impl Moment {
    pub const fn from_millis(ms: u64) -> Self {
        Moment(ms)
    }

    pub const fn millis(self) -> u64 {
        self.0
    }

    /// The zero moment (an arbitrary epoch start).
    pub const fn zero() -> Self {
        Moment(0)
    }

    /// The current monotonic time relative to a caller-owned epoch captured
    /// once at process start (the UI boundary conversion).
    pub fn from_elapsed(epoch: Instant) -> Self {
        Moment(epoch.elapsed().as_millis() as u64)
    }

    /// `self - other`, saturating at zero (never underflows; a release
    /// observed before its press is treated as a zero-duration hold).
    pub fn saturating_duration_since(self, other: Moment) -> Duration {
        Duration::from_millis(self.0.saturating_sub(other.0))
    }
}

impl Add<Duration> for Moment {
    type Output = Moment;
    fn add(self, rhs: Duration) -> Moment {
        Moment(self.0.saturating_add(rhs.as_millis() as u64))
    }
}

impl Sub<Duration> for Moment {
    type Output = Moment;
    fn sub(self, rhs: Duration) -> Moment {
        Moment(self.0.saturating_sub(rhs.as_millis() as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn millis_round_trip() {
        assert_eq!(Moment::from_millis(1234).millis(), 1234);
        assert_eq!(Moment::zero().millis(), 0);
    }

    #[test]
    fn ordering_and_duration_arithmetic() {
        let a = Moment::from_millis(100);
        let b = a + Duration::from_millis(50);
        assert!(b > a);
        assert_eq!(b.millis(), 150);
        assert_eq!((b - Duration::from_millis(40)).millis(), 110);
        assert_eq!(b.saturating_duration_since(a), Duration::from_millis(50));
        // Saturating: a release before its press never underflows.
        assert_eq!(a.saturating_duration_since(b), Duration::ZERO);
    }
}
