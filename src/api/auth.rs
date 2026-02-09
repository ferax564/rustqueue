//! Bearer token authentication middleware for the HTTP API.
//!
//! When `AuthConfig.enabled` is `true`, all requests (except public routes like
//! health and metrics) must include an `Authorization: Bearer <token>` header
//! with a token that appears in `AuthConfig.tokens`.
//!
//! Includes IP-based rate limiting: after `MAX_AUTH_FAILURES` failed attempts
//! within the lockout window, subsequent requests from the same IP are rejected
//! for `LOCKOUT_DURATION_SECS` seconds.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;

use crate::api::{AppState, ErrorDetail, ErrorResponse};
use crate::auth::validate_bearer_header;

/// Maximum auth failures before lockout.
const MAX_AUTH_FAILURES: u32 = 5;
/// How long to lock out an IP after too many failures (seconds).
const LOCKOUT_DURATION_SECS: u64 = 300; // 5 minutes
/// How often to prune stale entries from the tracker (seconds).
const PRUNE_INTERVAL_SECS: u64 = 60;

/// Per-IP auth failure tracking.
struct AuthFailureEntry {
    count: u32,
    first_failure: Instant,
    last_failure: Instant,
}

/// In-memory auth failure rate limiter.
///
/// Tracks failed authentication attempts per IP address and enforces lockouts
/// after too many failures. Thread-safe via `DashMap`.
pub struct AuthRateLimiter {
    failures: DashMap<IpAddr, AuthFailureEntry>,
    last_prune: std::sync::Mutex<Instant>,
}

impl AuthRateLimiter {
    pub fn new() -> Self {
        Self {
            failures: DashMap::new(),
            last_prune: std::sync::Mutex::new(Instant::now()),
        }
    }

    /// Check if an IP is currently locked out. Returns `true` if the request
    /// should be rejected.
    pub fn is_locked_out(&self, ip: &IpAddr) -> bool {
        if let Some(entry) = self.failures.get(ip) {
            if entry.count >= MAX_AUTH_FAILURES {
                let elapsed = entry.last_failure.elapsed().as_secs();
                if elapsed < LOCKOUT_DURATION_SECS {
                    return true;
                }
                // Lockout expired — will be pruned later
            }
        }
        false
    }

    /// Record a failed auth attempt for the given IP.
    pub fn record_failure(&self, ip: IpAddr) {
        let now = Instant::now();
        self.failures
            .entry(ip)
            .and_modify(|entry| {
                // Reset counter if the first failure was too long ago
                if entry.first_failure.elapsed().as_secs() > LOCKOUT_DURATION_SECS {
                    entry.count = 1;
                    entry.first_failure = now;
                } else {
                    entry.count += 1;
                }
                entry.last_failure = now;
            })
            .or_insert(AuthFailureEntry {
                count: 1,
                first_failure: now,
                last_failure: now,
            });

        self.maybe_prune();
    }

    /// Record a successful auth, clearing the failure counter.
    pub fn record_success(&self, ip: &IpAddr) {
        self.failures.remove(ip);
    }

    /// Periodically prune expired entries to prevent unbounded memory growth.
    fn maybe_prune(&self) {
        let mut last = self.last_prune.lock().unwrap();
        if last.elapsed().as_secs() < PRUNE_INTERVAL_SECS {
            return;
        }
        *last = Instant::now();
        drop(last);

        self.failures
            .retain(|_, entry| entry.last_failure.elapsed().as_secs() < LOCKOUT_DURATION_SECS);
    }
}

impl Default for AuthRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Axum middleware that enforces bearer-token authentication with rate limiting.
///
/// If `state.auth_config.enabled` is `false`, all requests pass through
/// unconditionally. When enabled, the middleware extracts the
/// `Authorization: Bearer <token>` header and checks it against the configured
/// token list. Invalid or missing tokens result in a 401 JSON error response.
///
/// After `MAX_AUTH_FAILURES` failed attempts from the same IP within the lockout
/// window, all requests from that IP are rejected with a 429 response.
pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    // If auth is disabled, pass through immediately.
    if !state.auth_config.enabled {
        return next.run(request).await;
    }

    // Extract client IP for rate limiting from the X-Forwarded-For header
    // or fall back to a default (when ConnectInfo is not available).
    let client_ip: Option<IpAddr> = request
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.trim().parse().ok())
        .or_else(|| {
            // Try to get from extensions (ConnectInfo) if available
            request
                .extensions()
                .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                .map(|ci| ci.0.ip())
        });

    // Check rate limit lockout.
    if let Some(ip) = client_ip {
        if state.auth_rate_limiter.is_locked_out(&ip) {
            return rate_limited_response();
        }
    }

    let auth_header = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());

    match validate_bearer_header(auth_header, &state.auth_config.tokens) {
        Ok(()) => {
            if let Some(ip) = client_ip {
                state.auth_rate_limiter.record_success(&ip);
            }
            next.run(request).await
        }
        Err(err) => {
            if let Some(ip) = client_ip {
                state.auth_rate_limiter.record_failure(ip);
            }
            unauthorized_response(err.message())
        }
    }
}

/// Build a 401 JSON error response matching the PRD error envelope format.
fn unauthorized_response(message: &str) -> Response {
    let body = ErrorResponse {
        ok: false,
        error: ErrorDetail {
            code: "UNAUTHORIZED",
            message: message.to_string(),
            details: None,
        },
    };
    (StatusCode::UNAUTHORIZED, Json(body)).into_response()
}

/// Build a 429 JSON error response for rate-limited requests.
fn rate_limited_response() -> Response {
    let body = ErrorResponse {
        ok: false,
        error: ErrorDetail {
            code: "RATE_LIMITED",
            message: "Too many failed authentication attempts. Try again later.".to_string(),
            details: None,
        },
    };
    (StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response()
}
