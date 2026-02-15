//! Per-queue token-bucket rate limiter.
//!
//! Provides [`QueueRateLimiter`] which enforces sustained-rate + burst limits
//! on a per-queue basis using a classic token-bucket algorithm. Unconfigured
//! queues are unlimited.

use std::sync::Mutex;
use std::time::Instant;

use dashmap::DashMap;

/// Per-queue rate limiter backed by independent token buckets.
///
/// Thread-safe: each bucket is protected by its own `Mutex`, and the
/// overall map uses `DashMap` for lock-free concurrent access to
/// different queues.
pub struct QueueRateLimiter {
    buckets: DashMap<String, Mutex<TokenBucket>>,
}

/// Internal token-bucket state for a single queue.
struct TokenBucket {
    /// Current number of available tokens (can be fractional during refill).
    tokens: f64,
    /// Maximum tokens the bucket can hold (burst capacity).
    max_tokens: f64,
    /// Tokens added per second (sustained rate).
    refill_rate: f64,
    /// Last time tokens were refilled.
    last_refill: Instant,
}

impl TokenBucket {
    fn new(rate_per_second: f64, burst: u32) -> Self {
        let max_tokens = burst as f64;
        Self {
            tokens: max_tokens,
            max_tokens,
            refill_rate: rate_per_second,
            last_refill: Instant::now(),
        }
    }

    /// Refill tokens based on elapsed time, then try to consume one token.
    /// Returns `true` if the request is allowed, `false` if rate limited.
    fn try_consume(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.last_refill = now;

        // Refill tokens proportionally to elapsed time.
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.max_tokens);

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

impl QueueRateLimiter {
    /// Create a new rate limiter with no queues configured.
    pub fn new() -> Self {
        Self {
            buckets: DashMap::new(),
        }
    }

    /// Configure a rate limit for the given queue.
    ///
    /// - `rate_per_second`: sustained push rate (tokens refilled per second).
    /// - `burst`: maximum tokens the bucket can hold. When `None`, defaults to
    ///   `rate_per_second` (i.e. one second of burst).
    pub fn configure(&self, queue: &str, rate_per_second: f64, burst: Option<u32>) {
        let burst = burst.unwrap_or(rate_per_second.ceil() as u32);
        self.buckets.insert(
            queue.to_string(),
            Mutex::new(TokenBucket::new(rate_per_second, burst)),
        );
    }

    /// Check whether a push to `queue` is allowed.
    ///
    /// Returns `true` if the push should proceed, `false` if rate limited.
    /// Queues without a configured rate limit always return `true`.
    pub fn check(&self, queue: &str) -> bool {
        let Some(entry) = self.buckets.get(queue) else {
            return true; // unconfigured = unlimited
        };
        let mut bucket = entry.lock().unwrap();
        bucket.try_consume()
    }
}

impl Default for QueueRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_allows_within_rate() {
        let rl = QueueRateLimiter::new();
        rl.configure("emails", 10.0, Some(5));

        // Should allow up to burst (5 tokens).
        for i in 0..5 {
            assert!(rl.check("emails"), "request {i} should be allowed");
        }
    }

    #[test]
    fn test_rejects_over_burst() {
        let rl = QueueRateLimiter::new();
        rl.configure("emails", 10.0, Some(3));

        // Consume all 3 burst tokens.
        assert!(rl.check("emails"));
        assert!(rl.check("emails"));
        assert!(rl.check("emails"));

        // 4th request should be rejected.
        assert!(!rl.check("emails"), "should reject over burst");
    }

    #[test]
    fn test_unlimited_queues_pass() {
        let rl = QueueRateLimiter::new();
        // "unknown" queue has no configuration — should always pass.
        for _ in 0..100 {
            assert!(rl.check("unknown"));
        }
    }

    #[test]
    fn test_refills_over_time() {
        let rl = QueueRateLimiter::new();
        // 10 tokens/sec, burst of 2.
        rl.configure("slow", 10.0, Some(2));

        // Exhaust burst.
        assert!(rl.check("slow"));
        assert!(rl.check("slow"));
        assert!(!rl.check("slow"));

        // Wait 200ms — should refill ~2 tokens (10/sec * 0.2s = 2).
        thread::sleep(Duration::from_millis(200));

        assert!(rl.check("slow"), "should allow after refill");
    }

    #[test]
    fn test_burst_defaults_to_rate() {
        let rl = QueueRateLimiter::new();
        // rate=5.0, burst=None → burst defaults to ceil(5.0) = 5.
        rl.configure("default-burst", 5.0, None);

        for i in 0..5 {
            assert!(rl.check("default-burst"), "request {i} should be allowed");
        }
        assert!(
            !rl.check("default-burst"),
            "should reject when burst exhausted"
        );
    }

    #[test]
    fn test_independent_queues() {
        let rl = QueueRateLimiter::new();
        rl.configure("a", 1.0, Some(1));
        rl.configure("b", 1.0, Some(1));

        // Exhaust queue "a".
        assert!(rl.check("a"));
        assert!(!rl.check("a"));

        // Queue "b" should be unaffected.
        assert!(rl.check("b"));
    }
}
