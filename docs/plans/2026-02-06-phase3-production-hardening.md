# RustQueue Phase 3: Production Hardening — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task.

**Goal:** Make RustQueue production-ready with authentication, graceful shutdown, automatic job retention cleanup, HTTP middleware (CORS, tracing), TLS for TCP, and an embedded web dashboard — all the operational features that separate a toy project from an industrial-grade job scheduler.

**Architecture:** Phase 3 adds six cross-cutting operational concerns on top of the existing Phase 1-2 foundation: (1) bearer token auth as axum middleware + TCP connection-level auth, (2) graceful shutdown with in-flight request draining, (3) retention-based auto-cleanup in the scheduler tick loop, (4) tower-http middleware (CORS + request tracing), (5) TLS for the TCP protocol via tokio-rustls, and (6) a single-page embedded web dashboard using rust-embed with a vanilla HTML/CSS/JS UI that consumes the existing HTTP API.

**Tech Stack:** Rust 2024, axum 0.8, tower-http 0.6 (cors+trace), tokio-rustls 0.26, rust-embed 8, existing RustQueue HTTP API

---

## Cargo.toml Changes (Phase 3)

No new dependencies needed — all deps already exist in Cargo.toml:
- `tower-http = { version = "0.6", features = ["cors", "trace"] }` — already compiled, needs wiring
- `rustls = { version = "0.23", optional = true }` — behind `tls` feature
- `tokio-rustls = { version = "0.26", optional = true }` — behind `tls` feature
- `rust-embed = { version = "8", features = ["interpolate-folder-path"] }` — already compiled

Only change: add `rcgen` as a dev-dependency for TLS test certificate generation.

---

## Detailed Phase 3: 10 Tasks

---

### Task 1: Bearer Token Auth Middleware for HTTP

**Files:**
- Create: `src/api/auth.rs`
- Modify: `src/api/mod.rs` — add `pub mod auth;`, wire middleware into router
- Modify: `src/api/mod.rs:AppState` — add `auth_config: AuthConfig`
- Modify: `src/main.rs` — pass auth config into AppState

**Implementation:**

Create an axum middleware layer that:
- Extracts the `Authorization: Bearer <token>` header
- Compares it against `AppState.auth_config.tokens`
- If `auth_config.enabled == false`, all requests pass through
- If enabled and token is missing/invalid, return 401 with `RustQueueError::Unauthorized`
- Health check (`/api/v1/health`) and Prometheus metrics (`/api/v1/metrics/prometheus`) are always public (no auth)

The middleware function signature:
```rust
pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response
```

Router wiring — split into public (health, metrics) and protected (everything else) route groups:
```rust
pub fn router(state: Arc<AppState>) -> axum::Router {
    let public = axum::Router::new()
        .merge(health::routes())
        .merge(prometheus::routes());

    let protected = axum::Router::new()
        .merge(jobs::routes())
        .merge(queues::routes())
        .merge(websocket::routes())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::auth_middleware,
        ));

    public.merge(protected).with_state(state)
}
```

AppState gets `auth_config: crate::config::AuthConfig` field. Update main.rs to pass `config.auth.clone()` into AppState. Update all test files that construct AppState to include the new field (with `AuthConfig::default()` which has `enabled: false`).

**Tests (4 tests in `tests/auth_tests.rs`):**
- `test_auth_disabled_allows_all` — with `enabled: false`, all endpoints accessible without token
- `test_auth_enabled_rejects_no_token` — with `enabled: true`, returns 401 without Bearer header
- `test_auth_enabled_rejects_bad_token` — with wrong token, returns 401
- `test_auth_enabled_accepts_valid_token` — with correct token, returns 200
- `test_health_always_public` — health endpoint accessible even with auth enabled and no token

**Commit:** `feat(auth): add bearer token authentication middleware for HTTP API`

---

### Task 2: TCP Authentication

**Files:**
- Modify: `src/protocol/handler.rs` — add `auth` command handling and connection-level auth state
- Modify: `src/protocol/mod.rs` — pass auth config to connection handler

**Implementation:**

Add an `auth` command to the TCP protocol. When auth is enabled:
- First command on any connection must be `{"cmd":"auth","token":"<bearer-token>"}`
- If token is valid, respond `{"ok":true}` and set `authenticated = true` for the connection
- All subsequent commands are allowed
- If any non-auth command arrives before authentication, respond with `{"ok":false,"error":{"code":"UNAUTHORIZED","message":"Authentication required. Send {\"cmd\":\"auth\",\"token\":\"...\"} first"}}`
- If auth is disabled in config, all commands are allowed without auth

Modify `handle_connection` to accept `auth_config: &AuthConfig` parameter and track auth state:
```rust
pub async fn handle_connection(
    stream: TcpStream,
    manager: Arc<QueueManager>,
    auth_config: &AuthConfig,
) {
    let mut authenticated = !auth_config.enabled; // skip auth if disabled
    // ... in the command loop, check authenticated before processing
}
```

Update `start_tcp_server` to accept and pass `AuthConfig`.
Update main.rs to pass auth config to TCP server.

**Tests (3 tests in `tests/tcp_auth_tests.rs`):**
- `test_tcp_auth_disabled_allows_commands` — commands work without auth when disabled
- `test_tcp_auth_required_rejects_unauthenticated` — returns UNAUTHORIZED without auth
- `test_tcp_auth_flow` — auth command succeeds, subsequent commands work

**Commit:** `feat(auth): add TCP connection-level bearer token authentication`

---

### Task 3: Tower-HTTP Middleware — CORS + Request Tracing

**Files:**
- Modify: `src/api/mod.rs` — add CORS and trace layers to router

**Implementation:**

Wire the already-compiled tower-http features into the router:

```rust
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

pub fn router(state: Arc<AppState>) -> axum::Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // ... existing route composition ...

    public
        .merge(protected)
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
```

CORS allows all origins by default (can be restricted in config later). TraceLayer adds structured spans for every HTTP request automatically.

**Tests (2 tests):**
- `test_cors_preflight_returns_headers` — OPTIONS request returns CORS headers
- `test_requests_produce_trace_spans` — verify trace middleware doesn't break normal requests

**Commit:** `feat(api): add CORS and request tracing middleware`

---

### Task 4: Graceful Shutdown

**Files:**
- Modify: `src/main.rs` — replace `abort()` with graceful shutdown coordination
- Add: shutdown token/signal sharing between components

**Implementation:**

Replace the current hard-abort shutdown with a graceful approach:

1. Use `tokio_util::sync::CancellationToken` (already available via tokio) or a simpler `tokio::sync::watch` channel for shutdown signaling
2. HTTP: Use axum's `serve().with_graceful_shutdown()` to drain in-flight requests
3. TCP: Stop accepting new connections, allow in-flight commands to complete
4. Scheduler: Clean exit from tick loop

Actually, we'll use `tokio::sync::watch` since it's already in tokio:

```rust
let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

// HTTP server with graceful shutdown
let http_handle = tokio::spawn({
    let mut rx = shutdown_rx.clone();
    async move {
        axum::serve(http_listener, app)
            .with_graceful_shutdown(async move {
                rx.changed().await.ok();
            })
            .await
            .expect("HTTP server error");
    }
});

// On Ctrl+C
tokio::signal::ctrl_c().await?;
info!("Shutdown signal received, draining connections...");
shutdown_tx.send(true)?;

// Wait for servers with timeout
let _ = tokio::time::timeout(
    std::time::Duration::from_secs(30),
    futures_util::future::join_all(vec![http_handle, tcp_handle])
).await;

scheduler_handle.abort(); // scheduler can be aborted safely
```

For TCP: modify `start_tcp_server` to accept a `watch::Receiver<bool>` and stop the accept loop when signaled. In-flight connections continue until they finish their current command.

**Tests (1 test in `tests/graceful_shutdown.rs`):**
- `test_graceful_shutdown_completes_inflight` — start server, push a job, send shutdown signal, verify job was persisted

**Commit:** `feat: add graceful shutdown with connection draining`

---

### Task 5: Retention Auto-Cleanup

**Files:**
- Modify: `src/engine/queue.rs` — add `cleanup_expired_jobs()` method
- Modify: `src/engine/scheduler.rs` — call cleanup in tick loop (every N ticks, not every tick)
- Modify: `src/storage/mod.rs` — add `remove_failed_before()` and `remove_dlq_before()` methods to trait
- Implement new trait methods in all 4 backends (redb, memory, sqlite, postgres)

**Implementation:**

Add a TTL parsing utility (in config module or queue manager):
```rust
fn parse_ttl(ttl: &str) -> Option<chrono::Duration> {
    let s = ttl.trim();
    if let Some(d) = s.strip_suffix('d') {
        d.parse::<i64>().ok().map(chrono::Duration::days)
    } else if let Some(h) = s.strip_suffix('h') {
        h.parse::<i64>().ok().map(chrono::Duration::hours)
    } else if let Some(m) = s.strip_suffix('m') {
        m.parse::<i64>().ok().map(chrono::Duration::minutes)
    } else {
        None
    }
}
```

New `StorageBackend` methods:
```rust
async fn remove_failed_before(&self, before: DateTime<Utc>) -> anyhow::Result<u64>;
async fn remove_dlq_before(&self, before: DateTime<Utc>) -> anyhow::Result<u64>;
```

`cleanup_expired_jobs()` on `QueueManager`:
```rust
pub async fn cleanup_expired_jobs(
    &self,
    completed_ttl: &str,
    failed_ttl: &str,
    dlq_ttl: &str,
) -> Result<(u64, u64, u64), RustQueueError> {
    let now = Utc::now();
    let completed = if let Some(dur) = parse_ttl(completed_ttl) {
        self.storage.remove_completed_before(now - dur).await.map_err(RustQueueError::Internal)?
    } else { 0 };
    let failed = if let Some(dur) = parse_ttl(failed_ttl) {
        self.storage.remove_failed_before(now - dur).await.map_err(RustQueueError::Internal)?
    } else { 0 };
    let dlq = if let Some(dur) = parse_ttl(dlq_ttl) {
        self.storage.remove_dlq_before(now - dur).await.map_err(RustQueueError::Internal)?
    } else { 0 };
    Ok((completed, failed, dlq))
}
```

In `scheduler.rs`, modify `start_scheduler` to accept retention config and run cleanup every 60 ticks (once per minute at default 1s tick):
```rust
pub fn start_scheduler(
    manager: Arc<QueueManager>,
    tick_interval_ms: u64,
    stall_timeout_ms: u64,
    retention: RetentionConfig,
) -> tokio::task::JoinHandle<()> {
    // ...
    let mut tick_count: u64 = 0;
    loop {
        ticker.tick().await;
        tick_count += 1;
        // ... existing promote, timeout, stall checks ...

        // Cleanup every 60 ticks
        if tick_count % 60 == 0 {
            if let Err(e) = manager.cleanup_expired_jobs(
                &retention.completed_ttl,
                &retention.failed_ttl,
                &retention.dlq_ttl,
            ).await {
                warn!(error = %e, "Retention cleanup failed");
            }
        }
    }
}
```

Update main.rs to pass `config.retention.clone()` to `start_scheduler`.

**Tests (3 tests):**
- `test_parse_ttl_formats` — unit test for TTL parsing (7d, 24h, 30m, invalid)
- `test_cleanup_removes_old_completed` — push, ack, set completed_at to past, run cleanup, verify deleted
- `test_cleanup_preserves_recent` — push, ack, run cleanup with 7d TTL, verify recent job preserved

**Commit:** `feat(engine): add retention-based auto-cleanup in scheduler`

---

### Task 6: TLS for TCP Protocol

**Files:**
- Modify: `src/config/mod.rs` — add `TlsConfig` with `cert_path` and `key_path`
- Modify: `src/protocol/mod.rs` — conditional TLS wrapping of TCP listener
- Modify: `src/main.rs` — load TLS config and create TLS acceptor

**Implementation:**

New config section:
```rust
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TlsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub cert_path: String,
    #[serde(default)]
    pub key_path: String,
}
```

Add to `RustQueueConfig`:
```rust
#[serde(default)]
pub tls: TlsConfig,
```

In `main.rs`, when TLS is enabled and `tls` feature is compiled:
```rust
#[cfg(feature = "tls")]
if config.tls.enabled {
    let certs = load_certs(&config.tls.cert_path)?;
    let key = load_key(&config.tls.key_path)?;
    let tls_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls_config));
    // Pass acceptor to TCP server
}
```

Modify `start_tcp_server` to optionally wrap streams with TLS:
```rust
#[cfg(feature = "tls")]
pub async fn start_tls_tcp_server(
    listener: TcpListener,
    manager: Arc<QueueManager>,
    auth_config: AuthConfig,
    acceptor: tokio_rustls::TlsAcceptor,
) {
    loop {
        let (stream, _) = listener.accept().await?;
        let tls_stream = acceptor.accept(stream).await?;
        // handle_connection with TlsStream
    }
}
```

The handler needs to be generic over `AsyncRead + AsyncWrite` instead of `TcpStream`:
```rust
pub async fn handle_connection<S>(
    stream: S,
    manager: Arc<QueueManager>,
    auth_config: &AuthConfig,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
```

**Tests (1 test behind `#[cfg(feature = "tls")]`):**
- `test_tls_tcp_connection` — generate self-signed cert with rcgen, start TLS TCP server, connect with TLS client, send push command

**Commit:** `feat(tls): add TLS support for TCP protocol behind tls feature flag`

---

### Task 7: Embedded Dashboard — Backend Routes

**Files:**
- Modify: `src/dashboard/mod.rs` — implement dashboard asset serving via rust-embed
- Modify: `src/api/mod.rs` — merge dashboard routes into router

**Implementation:**

Use rust-embed to serve static files from `dashboard/static/`:
```rust
use rust_embed::Embed;
use axum::{Router, response::Html, extract::Path as AxumPath};

#[derive(Embed)]
#[folder = "dashboard/static"]
struct DashboardAssets;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/dashboard", get(index))
        .route("/dashboard/{*path}", get(serve_asset))
}

async fn index() -> impl IntoResponse {
    serve_embedded("index.html")
}

async fn serve_asset(AxumPath(path): AxumPath<String>) -> impl IntoResponse {
    serve_embedded(&path)
}

fn serve_embedded(path: &str) -> impl IntoResponse {
    match DashboardAssets::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_text_plain();
            ([(axum::http::header::CONTENT_TYPE, mime.to_string())], file.data.to_vec()).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
```

Add `mime_guess` dependency to Cargo.toml (or use a simple match on file extension to avoid new deps):
Actually, avoid new deps. Use a simple helper:
```rust
fn content_type(path: &str) -> &str {
    if path.ends_with(".html") { "text/html; charset=utf-8" }
    else if path.ends_with(".css") { "text/css" }
    else if path.ends_with(".js") { "application/javascript" }
    else if path.ends_with(".json") { "application/json" }
    else if path.ends_with(".svg") { "image/svg+xml" }
    else if path.ends_with(".png") { "image/png" }
    else { "application/octet-stream" }
}
```

Wire into router in `src/api/mod.rs`:
```rust
pub fn router(state: Arc<AppState>) -> axum::Router {
    // ... existing public + protected ...
    let dashboard = crate::dashboard::routes();
    public.merge(protected).merge(dashboard).layer(cors).layer(TraceLayer::new_for_http()).with_state(state)
}
```

**Tests (2 tests):**
- `test_dashboard_index_returns_html` — GET /dashboard returns 200 with text/html
- `test_dashboard_missing_returns_404` — GET /dashboard/nonexistent returns 404

**Commit:** `feat(dashboard): add embedded asset serving via rust-embed`

---

### Task 8: Embedded Dashboard — Frontend HTML/CSS/JS

**Files:**
- Create: `dashboard/static/index.html` — single-page dashboard
- Create: `dashboard/static/style.css` — dashboard styles
- Create: `dashboard/static/app.js` — dashboard JavaScript

**Implementation:**

Build a clean, professional single-page dashboard that consumes the existing HTTP API:

**`index.html`** — Main layout with navigation sidebar and content panels:
- Header: RustQueue logo/name, server uptime, version
- Sidebar: Queues, Jobs, DLQ, Events links
- Main content: dynamic panels loaded by JS

**`style.css`** — Professional dark/light theme:
- CSS custom properties for theming
- Responsive grid layout
- Queue cards with colored state indicators
- Job table with sortable columns
- Status badges (Waiting=blue, Active=yellow, Completed=green, Failed=red, DLQ=purple)

**`app.js`** — Vanilla JS dashboard controller:
- `fetchQueues()` — GET /api/v1/queues, render queue cards with counts
- `fetchJob(id)` — GET /api/v1/jobs/:id, render job detail view
- Auto-refresh every 5 seconds
- WebSocket connection to /api/v1/events for live updates
- Event log panel showing real-time job lifecycle events
- Queue detail view: waiting/active/completed/failed counts as gauges
- No build step, no npm, no bundler — just vanilla HTML/CSS/JS

**Key UI Sections:**
1. **Overview** — Total queues, total jobs by state, throughput gauge
2. **Queues List** — Cards per queue showing counts
3. **Queue Detail** — Drill into a queue, see jobs
4. **Live Events** — Real-time event stream from WebSocket

**Commit:** `feat(dashboard): add embedded web dashboard with queue overview and live events`

---

### Task 9: Dashboard DLQ Management Panel

**Files:**
- Modify: `dashboard/static/app.js` — add DLQ panel
- Modify: `dashboard/static/index.html` — add DLQ nav link

**Implementation:**

Add a DLQ management panel to the dashboard:
- List DLQ jobs per queue (calls GET /api/v1/queues to find queues, then a new DLQ endpoint)
- Show job details: name, error, attempt count, timestamps
- This requires a new API endpoint for listing DLQ jobs

Add new HTTP endpoint:
- Modify `src/api/jobs.rs` — add `GET /api/v1/queues/{queue}/dlq` endpoint that calls `storage.get_dlq_jobs(queue, limit)`
- Wire route in `jobs::routes()`

**Tests (1 test):**
- `test_dlq_endpoint_returns_jobs` — push job, fail until DLQ, verify GET /api/v1/queues/work/dlq returns the job

**Commit:** `feat(dashboard): add DLQ management panel and DLQ list endpoint`

---

### Task 10: Final Integration + Smoke Tests

**Files:**
- Create: `tests/production_smoke.rs` — end-to-end production scenario test
- Modify: all test files that construct AppState — ensure `auth_config` field is included

**Implementation:**

Write a comprehensive integration test that exercises the full Phase 3 feature set:

1. Start server with auth enabled, retention configured
2. Authenticate via HTTP with bearer token
3. Push jobs, pull, ack, fail
4. Verify WebSocket events still work with auth
5. Verify dashboard is accessible at /dashboard
6. Verify health check is public (no auth needed)
7. Verify CORS preflight works

Also ensure `cargo test` passes cleanly with default features and `cargo test --features sqlite` passes.

Run `cargo clippy --features sqlite,postgres,otel` and fix any warnings.

**Tests (1 comprehensive test):**
- `test_production_scenario` — full lifecycle with auth enabled

**Commit:** `test: add production smoke test for Phase 3 features`

---

## Dependency Graph

```
Task 1 (HTTP Auth) ────────────────────────────────────────┐
    │                                                       │
Task 2 (TCP Auth) ◄──── Task 1 (reuses AuthConfig pattern) │
    │                                                       │
Task 3 (CORS + Tracing) ◄──── Task 1 (router changes)      │
    │                                                       │
Task 4 (Graceful Shutdown) ─── standalone                   │
    │                                                       │
Task 5 (Retention Cleanup) ─── standalone                   │
    │                                                       │
Task 6 (TLS) ◄──── Task 2 (TCP handler generic)            │
    │                                                       │
Task 7 (Dashboard Backend) ─── standalone                   │
    │                                                       │
Task 8 (Dashboard Frontend) ◄──── Task 7                    │
    │                                                       │
Task 9 (DLQ Panel) ◄──── Task 8                            │
    │                                                       │
Task 10 (Integration) ◄──── All tasks                       │
```

**Recommended execution order:**
1 → 2 → 3 → 4 → 5 → 6 → 7 → 8 → 9 → 10

---

## Phase 3 Exit Criteria

- [ ] `cargo test` — all tests pass (default features)
- [ ] `cargo test --features sqlite` — SQLite tests pass
- [ ] `cargo clippy --features sqlite,postgres,otel` — no warnings
- [ ] Auth middleware blocks unauthorized HTTP requests when enabled
- [ ] TCP auth command gates access to all TCP commands when enabled
- [ ] Health check and Prometheus metrics are always public
- [ ] CORS preflight returns proper headers
- [ ] Request tracing produces structured spans
- [ ] Graceful shutdown drains in-flight requests (30s timeout)
- [ ] Retention cleanup runs in scheduler, removes expired completed/failed/DLQ jobs
- [ ] TLS wraps TCP connections when configured (behind `tls` feature)
- [ ] Dashboard accessible at `/dashboard` with queue overview and live events
- [ ] DLQ management panel shows failed jobs
- [ ] No new dependencies added (except rcgen as dev-dep for TLS tests)

---

## Critical Files Reference

| File | Role | Phase 3 Changes |
|------|------|-----------------|
| `src/api/mod.rs` | Router + AppState | Auth middleware, CORS, tracing layers, dashboard routes, auth_config field |
| `src/api/auth.rs` | NEW: Auth middleware | Bearer token verification |
| `src/api/jobs.rs` | Job endpoints | DLQ list endpoint |
| `src/protocol/mod.rs` | TCP server | Auth config, TLS acceptor, graceful shutdown |
| `src/protocol/handler.rs` | TCP handler | Auth command, generic over AsyncRead+AsyncWrite |
| `src/config/mod.rs` | Configuration | TlsConfig struct |
| `src/engine/queue.rs` | QueueManager | cleanup_expired_jobs(), parse_ttl() |
| `src/engine/scheduler.rs` | Background scheduler | Retention cleanup every 60 ticks |
| `src/storage/mod.rs` | StorageBackend trait | remove_failed_before(), remove_dlq_before() |
| `src/dashboard/mod.rs` | Dashboard serving | rust-embed routes |
| `dashboard/static/` | Dashboard UI | index.html, style.css, app.js |
| `src/main.rs` | Server startup | Graceful shutdown, auth passing, TLS setup |
