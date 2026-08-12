//! Token-bucket rate limiting for daemon connections.
//!
//! A client that floods the daemon (rapid-fire key spam, hostile input) must
//! be bounded. The bucket is per-connection: `burst` tokens available
//! instantly, refilled at `per_second` per second.

use std::time::{Duration, Instant};

/// A token bucket.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    burst: f64,
    per_second: f64,
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    pub fn new(burst: u32, per_second: u32) -> Self {
        TokenBucket {
            burst: f64::from(burst.max(1)),
            per_second: f64::from(per_second.max(1)),
            tokens: f64::from(burst.max(1)),
            last: Instant::now(),
        }
    }

    /// Try to consume one token. Returns `false` when the bucket is empty
    /// (rate limit exceeded).
    pub fn allow(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.per_second).min(self.burst);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Time until the next token is available (for the server to decide how
    /// long to stall a violating client before dropping it).
    pub fn wait_time(&self) -> Duration {
        let missing = 1.0 - self.tokens;
        if missing <= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(missing / self.per_second)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_allows_instant_flood() {
        let mut bucket = TokenBucket::new(10, 1);
        for _ in 0..10 {
            assert!(bucket.allow());
        }
        // The burst is exhausted.
        assert!(!bucket.allow());
    }

    #[test]
    fn refills_over_time() {
        let mut bucket = TokenBucket::new(1, 10); // 10 tokens/second
        assert!(bucket.allow());
        assert!(!bucket.allow());
        std::thread::sleep(Duration::from_millis(150));
        assert!(bucket.allow(), "bucket should have refilled after 150ms");
    }

    #[test]
    fn never_exceeds_burst() {
        let mut bucket = TokenBucket::new(5, 100);
        std::thread::sleep(Duration::from_millis(200));
        let mut allowed = 0;
        for _ in 0..100 {
            if bucket.allow() {
                allowed += 1;
            }
        }
        assert!(allowed <= 5, "burst cap violated: {allowed} allowed");
    }

    #[test]
    fn wait_time_is_bounded() {
        let mut bucket = TokenBucket::new(1, 1);
        bucket.allow();
        let wait = bucket.wait_time();
        assert!(wait >= Duration::ZERO && wait <= Duration::from_secs(1));
    }
}
