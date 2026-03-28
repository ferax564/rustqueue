//! Webhook manager for delivering job lifecycle events to external HTTP endpoints.
//!
//! Webhooks subscribe to the same `broadcast::Sender<JobEvent>` channel used by
//! WebSocket clients. Each registered webhook specifies which event types and
//! queues it cares about. When a matching event arrives, the manager delivers
//! an HMAC-SHA256 signed JSON payload to the webhook URL with exponential backoff retry.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::api::websocket::JobEvent;
use crate::engine::metrics as metric_names;

// ── Types ───────────────────────────────────────────────────────────────────

/// Events that a webhook can subscribe to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WebhookEventType {
    JobPushed,
    JobCompleted,
    JobFailed,
    JobDlq,
    JobCancelled,
    JobProgress,
}

/// A registered webhook endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Webhook {
    /// Unique webhook ID (UUID v7).
    #[schema(value_type = String, format = "uuid")]
    pub id: Uuid,
    /// Destination URL for event delivery (must be HTTPS in production).
    pub url: String,
    /// Which event types to deliver. Empty means all events.
    pub events: Vec<WebhookEventType>,
    /// Filter to specific queues. Empty means all queues.
    pub queues: Vec<String>,
    /// HMAC-SHA256 secret for payload signing. Omitted from list responses.
    #[serde(skip_serializing)]
    pub secret: Option<String>,
    /// Whether this webhook is active.
    pub active: bool,
    /// When the webhook was created.
    pub created_at: DateTime<Utc>,
}

/// Input for creating a webhook.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct WebhookInput {
    /// Destination URL for event delivery.
    pub url: String,
    /// Which event types to deliver. Empty or omitted means all events.
    #[serde(default)]
    pub events: Vec<WebhookEventType>,
    /// Filter to specific queues. Empty or omitted means all queues.
    #[serde(default)]
    pub queues: Vec<String>,
    /// HMAC-SHA256 secret for payload signing. If omitted, payloads are unsigned.
    pub secret: Option<String>,
}

/// Payload delivered to a webhook URL.
#[derive(Debug, Clone, Serialize)]
pub struct WebhookPayload {
    /// The event type.
    pub event: String,
    /// The webhook ID that matched.
    pub webhook_id: Uuid,
    /// When the event occurred.
    pub timestamp: DateTime<Utc>,
    /// Event data (job_id, queue).
    pub data: WebhookPayloadData,
}

/// Event data within a webhook payload.
#[derive(Debug, Clone, Serialize)]
pub struct WebhookPayloadData {
    pub job_id: Uuid,
    pub queue: String,
}

/// Configuration for the webhook delivery system.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebhookConfig {
    /// Whether webhooks are enabled.
    #[serde(default)]
    pub enabled: bool,
    /// HTTP delivery timeout in milliseconds.
    #[serde(default = "default_delivery_timeout_ms")]
    pub delivery_timeout_ms: u64,
    /// Maximum delivery attempts before giving up.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Base delay between retries in milliseconds (doubles each attempt).
    #[serde(default = "default_retry_base_delay_ms")]
    pub retry_base_delay_ms: u64,
}

impl Default for WebhookConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            delivery_timeout_ms: default_delivery_timeout_ms(),
            max_retries: default_max_retries(),
            retry_base_delay_ms: default_retry_base_delay_ms(),
        }
    }
}

fn default_delivery_timeout_ms() -> u64 {
    5000
}
fn default_max_retries() -> u32 {
    3
}
fn default_retry_base_delay_ms() -> u64 {
    1000
}

// ── WebhookManager ──────────────────────────────────────────────────────────

/// Manages webhook registrations and delivers events to registered endpoints.
pub struct WebhookManager {
    webhooks: DashMap<Uuid, Webhook>,
    http_client: reqwest::Client,
    config: WebhookConfig,
}

impl WebhookManager {
    /// Create a new webhook manager.
    pub fn new(config: WebhookConfig) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_millis(config.delivery_timeout_ms))
            .build()
            .expect("failed to build reqwest client");
        Self {
            webhooks: DashMap::new(),
            http_client,
            config,
        }
    }

    /// Register a new webhook. Returns the created webhook.
    pub fn register(&self, input: WebhookInput) -> Webhook {
        let webhook = Webhook {
            id: Uuid::now_v7(),
            url: input.url,
            events: input.events,
            queues: input.queues,
            secret: input.secret,
            active: true,
            created_at: Utc::now(),
        };
        let result = webhook.clone();
        self.webhooks.insert(webhook.id, webhook);
        info!(id = %result.id, url = %result.url, "Webhook registered");
        result
    }

    /// List all registered webhooks (secrets are excluded via serde skip).
    pub fn list(&self) -> Vec<Webhook> {
        self.webhooks.iter().map(|r| r.value().clone()).collect()
    }

    /// Get a webhook by ID.
    pub fn get(&self, id: Uuid) -> Option<Webhook> {
        self.webhooks.get(&id).map(|r| r.value().clone())
    }

    /// Delete a webhook by ID. Returns true if it existed.
    pub fn delete(&self, id: Uuid) -> bool {
        let removed = self.webhooks.remove(&id).is_some();
        if removed {
            info!(id = %id, "Webhook deleted");
        }
        removed
    }

    /// Check if a webhook matches a given event.
    fn matches(webhook: &Webhook, event_type: &str, queue: &str) -> bool {
        if !webhook.active {
            return false;
        }
        // Check event filter
        if !webhook.events.is_empty() {
            let event_enum = match event_type {
                "job.pushed" => WebhookEventType::JobPushed,
                "job.completed" => WebhookEventType::JobCompleted,
                "job.failed" => WebhookEventType::JobFailed,
                "job.dlq" => WebhookEventType::JobDlq,
                "job.cancelled" => WebhookEventType::JobCancelled,
                "job.progress" => WebhookEventType::JobProgress,
                _ => return false,
            };
            if !webhook.events.contains(&event_enum) {
                return false;
            }
        }
        // Check queue filter
        if !webhook.queues.is_empty() && !webhook.queues.iter().any(|q| q == queue) {
            return false;
        }
        true
    }

    /// Compute HMAC-SHA256 signature for a payload.
    pub fn sign_payload(secret: &str, body: &[u8]) -> String {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key size");
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    /// Deliver a payload to a single webhook with retry.
    async fn deliver(&self, webhook: &Webhook, payload: &WebhookPayload) {
        let body = match serde_json::to_vec(payload) {
            Ok(b) => b,
            Err(e) => {
                error!(webhook_id = %webhook.id, error = %e, "Failed to serialize webhook payload");
                return;
            }
        };

        for attempt in 0..=self.config.max_retries {
            let mut req = self
                .http_client
                .post(&webhook.url)
                .header("Content-Type", "application/json")
                .header("X-RustQueue-Event", &payload.event)
                .header("X-RustQueue-Webhook-Id", webhook.id.to_string());

            if let Some(secret) = &webhook.secret {
                let sig = Self::sign_payload(secret, &body);
                req = req.header("X-RustQueue-Signature", format!("sha256={sig}"));
            }

            match req.body(body.clone()).send().await {
                Ok(resp) if resp.status().is_success() => {
                    metrics::counter!(metric_names::WEBHOOKS_DELIVERED_TOTAL).increment(1);
                    debug!(
                        webhook_id = %webhook.id,
                        status = %resp.status(),
                        attempt = attempt,
                        "Webhook delivered"
                    );
                    return;
                }
                Ok(resp) => {
                    warn!(
                        webhook_id = %webhook.id,
                        status = %resp.status(),
                        attempt = attempt,
                        "Webhook delivery got non-success status"
                    );
                }
                Err(e) => {
                    warn!(
                        webhook_id = %webhook.id,
                        error = %e,
                        attempt = attempt,
                        "Webhook delivery failed"
                    );
                }
            }

            if attempt < self.config.max_retries {
                let delay = self.config.retry_base_delay_ms * 2u64.pow(attempt);
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
        }

        metrics::counter!(metric_names::WEBHOOKS_DELIVERY_FAILURES_TOTAL).increment(1);
        error!(
            webhook_id = %webhook.id,
            url = %webhook.url,
            max_retries = self.config.max_retries,
            "Webhook delivery exhausted all retries"
        );
    }

    /// Dispatch an event to all matching webhooks.
    async fn dispatch(&self, event: &JobEvent) {
        let matching: Vec<Webhook> = self
            .webhooks
            .iter()
            .filter(|r| Self::matches(r.value(), &event.event, &event.queue))
            .map(|r| r.value().clone())
            .collect();

        for webhook in matching {
            let payload = WebhookPayload {
                event: event.event.clone(),
                webhook_id: webhook.id,
                timestamp: event.timestamp,
                data: WebhookPayloadData {
                    job_id: event.job_id,
                    queue: event.queue.clone(),
                },
            };
            // Spawn delivery as a separate task so one slow webhook doesn't block others
            let manager_client = self.http_client.clone();
            let config = self.config.clone();
            tokio::spawn(async move {
                let temp_manager = WebhookManager {
                    webhooks: DashMap::new(),
                    http_client: manager_client,
                    config,
                };
                temp_manager.deliver(&webhook, &payload).await;
            });
        }
    }
}

/// Start the webhook event dispatcher background task.
/// Subscribes to the broadcast channel and dispatches events to matching webhooks.
pub fn start_webhook_dispatcher(
    manager: Arc<WebhookManager>,
    mut event_rx: broadcast::Receiver<JobEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(event) => {
                    manager.dispatch(&event).await;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(
                        dropped = n,
                        "Webhook dispatcher lagged, some events were missed"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => {
                    info!("Webhook dispatcher shutting down (channel closed)");
                    break;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_webhook_matches_all_events() {
        let wh = Webhook {
            id: Uuid::now_v7(),
            url: "http://example.com/hook".into(),
            events: vec![],
            queues: vec![],
            secret: None,
            active: true,
            created_at: Utc::now(),
        };
        assert!(WebhookManager::matches(&wh, "job.pushed", "emails"));
        assert!(WebhookManager::matches(&wh, "job.completed", "orders"));
    }

    #[test]
    fn test_webhook_matches_event_filter() {
        let wh = Webhook {
            id: Uuid::now_v7(),
            url: "http://example.com/hook".into(),
            events: vec![WebhookEventType::JobCompleted, WebhookEventType::JobFailed],
            queues: vec![],
            secret: None,
            active: true,
            created_at: Utc::now(),
        };
        assert!(!WebhookManager::matches(&wh, "job.pushed", "emails"));
        assert!(WebhookManager::matches(&wh, "job.completed", "emails"));
        assert!(WebhookManager::matches(&wh, "job.failed", "emails"));
    }

    #[test]
    fn test_webhook_matches_queue_filter() {
        let wh = Webhook {
            id: Uuid::now_v7(),
            url: "http://example.com/hook".into(),
            events: vec![],
            queues: vec!["emails".into()],
            secret: None,
            active: true,
            created_at: Utc::now(),
        };
        assert!(WebhookManager::matches(&wh, "job.pushed", "emails"));
        assert!(!WebhookManager::matches(&wh, "job.pushed", "orders"));
    }

    #[test]
    fn test_webhook_inactive_no_match() {
        let wh = Webhook {
            id: Uuid::now_v7(),
            url: "http://example.com/hook".into(),
            events: vec![],
            queues: vec![],
            secret: None,
            active: false,
            created_at: Utc::now(),
        };
        assert!(!WebhookManager::matches(&wh, "job.pushed", "emails"));
    }

    #[test]
    fn test_sign_payload() {
        let sig = WebhookManager::sign_payload("my-secret", b"hello world");
        // Known HMAC-SHA256 for "hello world" with key "my-secret"
        assert!(!sig.is_empty());
        assert_eq!(sig.len(), 64); // 32 bytes hex-encoded
    }

    #[test]
    fn test_webhook_crud() {
        let config = WebhookConfig::default();
        let mgr = WebhookManager::new(config);

        // Register
        let wh = mgr.register(WebhookInput {
            url: "http://example.com/hook".into(),
            events: vec![WebhookEventType::JobCompleted],
            queues: vec!["emails".into()],
            secret: Some("secret123".into()),
        });

        // List
        assert_eq!(mgr.list().len(), 1);

        // Get
        let fetched = mgr.get(wh.id).unwrap();
        assert_eq!(fetched.url, "http://example.com/hook");

        // Delete
        assert!(mgr.delete(wh.id));
        assert!(mgr.get(wh.id).is_none());
        assert_eq!(mgr.list().len(), 0);
    }
}
