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

    /// Refill tokens based on elapsed time, then try to consume `n` tokens.
    ///
    /// This is all-or-nothing: if there are not enough tokens for the full
    /// request, no tokens are consumed and `false` is returned.
    fn try_consume_n(&mut self, n: u32) -> bool {
        let cost = n as f64;
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.last_refill = now;

        // Refill tokens proportionally to elapsed time.
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.max_tokens);

        if self.tokens >= cost {
            self.tokens -= cost;
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
    ///   Must be finite and greater than zero.
    /// - `burst`: maximum tokens the bucket can hold. When `None`, defaults to
    ///   `rate_per_second` (i.e. one second of burst). When `Some`, must be > 0.
    ///
    /// Returns `Err` with a description if the inputs are invalid.
    pub fn configure(
        &self,
        queue: &str,
        rate_per_second: f64,
        burst: Option<u32>,
    ) -> Result<(), String> {
        if !rate_per_second.is_finite() || rate_per_second <= 0.0 {
            return Err(format!(
                "rate_per_second must be finite and > 0, got {rate_per_second}"
            ));
        }
        if let Some(b) = burst {
            if b == 0 {
                return Err("burst must be > 0".to_string());
            }
        }
        let burst = burst.unwrap_or(rate_per_second.ceil() as u32);
        self.buckets.insert(
            queue.to_string(),
            Mutex::new(TokenBucket::new(rate_per_second, burst)),
        );
        Ok(())
    }

    /// Check whether a single push to `queue` is allowed.
    ///
    /// Returns `true` if the push should proceed, `false` if rate limited.
    /// Queues without a configured rate limit always return `true`.
    pub fn check(&self, queue: &str) -> bool {
        self.check_n(queue, 1)
    }

    /// Check whether `n` pushes to `queue` are allowed.
    ///
    /// This is all-or-nothing: either all `n` tokens are consumed or none are.
    /// Queues without a configured rate limit always return `true`.
    pub fn check_n(&self, queue: &str, n: u32) -> bool {
        if n == 0 {
            return true;
        }
        let Some(entry) = self.buckets.get(queue) else {
            return true; // unconfigured = unlimited
        };
        let mut bucket = entry.lock().unwrap();
        bucket.try_consume_n(n)
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
        rl.configure("emails", 10.0, Some(5)).unwrap();

        // Should allow up to burst (5 tokens).
        for i in 0..5 {
            assert!(rl.check("emails"), "request {i} should be allowed");
        }
    }

    #[test]
    fn test_rejects_over_burst() {
        let rl = QueueRateLimiter::new();
        rl.configure("emails", 10.0, Some(3)).unwrap();

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
        rl.configure("slow", 10.0, Some(2)).unwrap();

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
        rl.configure("default-burst", 5.0, None).unwrap();

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
        rl.configure("a", 1.0, Some(1)).unwrap();
        rl.configure("b", 1.0, Some(1)).unwrap();

        // Exhaust queue "a".
        assert!(rl.check("a"));
        assert!(!rl.check("a"));

        // Queue "b" should be unaffected.
        assert!(rl.check("b"));
    }

    // ── check_n tests ────────────────────────────────────────────────────

    #[test]
    fn test_check_n_consumes_multiple_tokens() {
        let rl = QueueRateLimiter::new();
        rl.configure("batch", 100.0, Some(10)).unwrap();

        // Consume 7 of 10 tokens in one call.
        assert!(rl.check_n("batch", 7));
        // Only 3 left — requesting 4 should fail (all-or-nothing).
        assert!(!rl.check_n("batch", 4));
        // But 3 should succeed.
        assert!(rl.check_n("batch", 3));
    }

    #[test]
    fn test_check_n_zero_always_passes() {
        let rl = QueueRateLimiter::new();
        rl.configure("q", 1.0, Some(1)).unwrap();
        // Exhaust the single token.
        assert!(rl.check("q"));
        assert!(!rl.check("q"));
        // check_n(0) should still pass — no tokens consumed.
        assert!(rl.check_n("q", 0));
    }

    #[test]
    fn test_check_n_all_or_nothing() {
        let rl = QueueRateLimiter::new();
        rl.configure("aon", 100.0, Some(5)).unwrap();

        // Request more than available — should fail without consuming any.
        assert!(!rl.check_n("aon", 6));
        // All 5 tokens should still be available.
        assert!(rl.check_n("aon", 5));
    }

    #[test]
    fn test_check_n_unlimited_queue() {
        let rl = QueueRateLimiter::new();
        // No configuration for "free" — should always pass any N.
        assert!(rl.check_n("free", 1_000_000));
    }

    // ── Input validation tests ───────────────────────────────────────────

    #[test]
    fn test_configure_rejects_zero_rate() {
        let rl = QueueRateLimiter::new();
        let err = rl.configure("q", 0.0, None).unwrap_err();
        assert!(err.contains("rate_per_second"), "error: {err}");
    }

    #[test]
    fn test_configure_rejects_negative_rate() {
        let rl = QueueRateLimiter::new();
        let err = rl.configure("q", -5.0, None).unwrap_err();
        assert!(err.contains("rate_per_second"), "error: {err}");
    }

    #[test]
    fn test_configure_rejects_nan() {
        let rl = QueueRateLimiter::new();
        let err = rl.configure("q", f64::NAN, None).unwrap_err();
        assert!(err.contains("rate_per_second"), "error: {err}");
    }

    #[test]
    fn test_configure_rejects_infinity() {
        let rl = QueueRateLimiter::new();
        let err = rl.configure("q", f64::INFINITY, None).unwrap_err();
        assert!(err.contains("rate_per_second"), "error: {err}");
    }

    #[test]
    fn test_configure_rejects_neg_infinity() {
        let rl = QueueRateLimiter::new();
        let err = rl.configure("q", f64::NEG_INFINITY, None).unwrap_err();
        assert!(err.contains("rate_per_second"), "error: {err}");
    }

    #[test]
    fn test_configure_rejects_zero_burst() {
        let rl = QueueRateLimiter::new();
        let err = rl.configure("q", 10.0, Some(0)).unwrap_err();
        assert!(err.contains("burst"), "error: {err}");
    }

    #[test]
    fn test_configure_accepts_valid_inputs() {
        let rl = QueueRateLimiter::new();
        assert!(rl.configure("q1", 0.001, None).is_ok());
        assert!(rl.configure("q2", 1_000_000.0, Some(1)).is_ok());
        assert!(rl.configure("q3", 1.0, Some(u32::MAX)).is_ok());
    }
}
