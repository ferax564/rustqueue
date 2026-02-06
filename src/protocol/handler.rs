//! TCP connection handler — reads newline-delimited JSON commands, dispatches
//! to [`QueueManager`], and writes JSON responses.

use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tracing::{debug, warn};

use crate::config::AuthConfig;
use crate::engine::queue::{JobOptions, QueueManager};

/// Handle a single TCP connection, processing commands until the client disconnects.
///
/// When `auth_config.enabled` is `true`, the first command on the connection must
/// be `{"cmd":"auth","token":"<bearer-token>"}`. All subsequent commands are allowed
/// only after successful authentication. When auth is disabled, all commands are
/// allowed without an auth handshake.
pub async fn handle_connection(
    stream: TcpStream,
    manager: Arc<QueueManager>,
    auth_config: &AuthConfig,
) {
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    debug!(peer = %peer, "New TCP connection");

    // If auth is disabled, the connection starts as authenticated.
    let mut authenticated = !auth_config.enabled;

    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => {
                debug!(peer = %peer, "Client disconnected");
                break;
            }
            Ok(_) => {
                let response = if authenticated {
                    process_line(&line, &manager).await
                } else {
                    process_auth_line(&line, auth_config, &mut authenticated)
                };
                let mut resp_bytes = serde_json::to_string(&response).unwrap_or_else(|e| {
                    json!({"ok": false, "error": {"code": "INTERNAL_ERROR", "message": e.to_string()}})
                        .to_string()
                });
                resp_bytes.push('\n');
                if writer.write_all(resp_bytes.as_bytes()).await.is_err() {
                    debug!(peer = %peer, "Write failed, closing connection");
                    break;
                }
            }
            Err(e) => {
                warn!(peer = %peer, error = %e, "Read error, closing connection");
                break;
            }
        }
    }
}

/// Process a line when the connection is not yet authenticated.
///
/// Only the `auth` command is accepted. If the token is valid, `authenticated` is
/// set to `true` and `{"ok":true}` is returned. Any other command returns an
/// `UNAUTHORIZED` error prompting the client to authenticate first.
fn process_auth_line(line: &str, auth_config: &AuthConfig, authenticated: &mut bool) -> Value {
    let parsed: Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(e) => {
            return error_response("PARSE_ERROR", &format!("Invalid JSON: {e}"));
        }
    };

    let cmd = match parsed.get("cmd").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return error_response("VALIDATION_ERROR", "Missing or invalid 'cmd' field");
        }
    };

    if cmd != "auth" {
        return error_response(
            "UNAUTHORIZED",
            "Authentication required. Send {\"cmd\":\"auth\",\"token\":\"...\"} first",
        );
    }

    let token = match parsed.get("token").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => {
            return error_response("VALIDATION_ERROR", "Missing 'token' field in auth command");
        }
    };

    if auth_config.tokens.contains(&token.to_string()) {
        *authenticated = true;
        json!({"ok": true})
    } else {
        error_response("UNAUTHORIZED", "Invalid authentication token")
    }
}

/// Parse a single line as JSON and dispatch the command.
async fn process_line(line: &str, manager: &QueueManager) -> Value {
    let parsed: Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(e) => {
            return error_response("PARSE_ERROR", &format!("Invalid JSON: {e}"));
        }
    };

    let cmd = match parsed.get("cmd").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return error_response("VALIDATION_ERROR", "Missing or invalid 'cmd' field");
        }
    };

    match cmd {
        "push" => handle_push(&parsed, manager).await,
        "pull" => handle_pull(&parsed, manager).await,
        "ack" => handle_ack(&parsed, manager).await,
        "fail" => handle_fail(&parsed, manager).await,
        "cancel" => handle_cancel(&parsed, manager).await,
        "progress" => handle_progress(&parsed, manager).await,
        "heartbeat" => handle_heartbeat(&parsed, manager).await,
        "stats" => handle_stats(&parsed, manager).await,
        _ => error_response("UNKNOWN_COMMAND", &format!("Unknown command: {cmd}")),
    }
}

// ── Command handlers ─────────────────────────────────────────────────────────

async fn handle_push(cmd: &Value, manager: &QueueManager) -> Value {
    let queue = match cmd.get("queue").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return error_response("VALIDATION_ERROR", "Missing 'queue' field"),
    };
    let name = match cmd.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return error_response("VALIDATION_ERROR", "Missing 'name' field"),
    };
    let data = cmd.get("data").cloned().unwrap_or(json!({}));

    let options: Option<JobOptions> = cmd
        .get("options")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    match manager.push(queue, name, data, options).await {
        Ok(id) => json!({"ok": true, "id": id.to_string()}),
        Err(e) => engine_error_response(&e),
    }
}

async fn handle_pull(cmd: &Value, manager: &QueueManager) -> Value {
    let queue = match cmd.get("queue").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return error_response("VALIDATION_ERROR", "Missing 'queue' field"),
    };
    let count = cmd
        .get("count")
        .and_then(|v| v.as_u64())
        .unwrap_or(1) as u32;

    match manager.pull(queue, count).await {
        Ok(jobs) => {
            if count == 1 {
                // Single-job mode: return the job directly (or null).
                let job_val = jobs.into_iter().next().map(|j| json!(j)).unwrap_or(Value::Null);
                json!({"ok": true, "job": job_val})
            } else {
                let jobs_val: Vec<Value> = jobs.into_iter().map(|j| json!(j)).collect();
                json!({"ok": true, "jobs": jobs_val})
            }
        }
        Err(e) => engine_error_response(&e),
    }
}

async fn handle_ack(cmd: &Value, manager: &QueueManager) -> Value {
    let id_str = match cmd.get("id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return error_response("VALIDATION_ERROR", "Missing 'id' field"),
    };
    let id = match uuid::Uuid::parse_str(id_str) {
        Ok(id) => id,
        Err(_) => return error_response("VALIDATION_ERROR", "Invalid job ID format"),
    };
    let result = cmd.get("result").cloned();

    match manager.ack(id, result).await {
        Ok(()) => json!({"ok": true}),
        Err(e) => engine_error_response(&e),
    }
}

async fn handle_fail(cmd: &Value, manager: &QueueManager) -> Value {
    let id_str = match cmd.get("id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return error_response("VALIDATION_ERROR", "Missing 'id' field"),
    };
    let id = match uuid::Uuid::parse_str(id_str) {
        Ok(id) => id,
        Err(_) => return error_response("VALIDATION_ERROR", "Invalid job ID format"),
    };
    let error = cmd
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown error");

    match manager.fail(id, error).await {
        Ok(result) => {
            let next = result
                .next_attempt_at
                .map(|t| Value::String(t.to_rfc3339()));
            json!({
                "ok": true,
                "will_retry": result.will_retry,
                "next_attempt_at": next.unwrap_or(Value::Null),
            })
        }
        Err(e) => engine_error_response(&e),
    }
}

async fn handle_cancel(cmd: &Value, manager: &QueueManager) -> Value {
    let id_str = match cmd.get("id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return error_response("VALIDATION_ERROR", "Missing 'id' field"),
    };
    let id = match uuid::Uuid::parse_str(id_str) {
        Ok(id) => id,
        Err(_) => return error_response("VALIDATION_ERROR", "Invalid job ID format"),
    };

    match manager.cancel(id).await {
        Ok(()) => json!({"ok": true}),
        Err(e) => engine_error_response(&e),
    }
}

async fn handle_progress(cmd: &Value, manager: &QueueManager) -> Value {
    let id_str = match cmd.get("id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return error_response("VALIDATION_ERROR", "Missing 'id' field"),
    };
    let id = match uuid::Uuid::parse_str(id_str) {
        Ok(id) => id,
        Err(_) => return error_response("VALIDATION_ERROR", "Invalid job ID format"),
    };
    let progress = match cmd.get("progress").and_then(|v| v.as_u64()) {
        Some(p) => p as u8,
        None => return error_response("VALIDATION_ERROR", "Missing 'progress' field"),
    };
    let message = cmd
        .get("message")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    match manager.update_progress(id, progress, message).await {
        Ok(()) => json!({"ok": true}),
        Err(e) => engine_error_response(&e),
    }
}

async fn handle_heartbeat(cmd: &Value, manager: &QueueManager) -> Value {
    let id_str = match cmd.get("id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return error_response("VALIDATION_ERROR", "Missing 'id' field"),
    };
    let id = match uuid::Uuid::parse_str(id_str) {
        Ok(id) => id,
        Err(_) => return error_response("VALIDATION_ERROR", "Invalid job ID format"),
    };

    match manager.heartbeat(id).await {
        Ok(()) => json!({"ok": true}),
        Err(e) => engine_error_response(&e),
    }
}

async fn handle_stats(cmd: &Value, manager: &QueueManager) -> Value {
    let queue = match cmd.get("queue").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return error_response("VALIDATION_ERROR", "Missing 'queue' field"),
    };

    match manager.get_queue_stats(queue).await {
        Ok(counts) => json!({"ok": true, "counts": counts}),
        Err(e) => engine_error_response(&e),
    }
}

// ── Response helpers ─────────────────────────────────────────────────────────

fn error_response(code: &str, message: &str) -> Value {
    json!({
        "ok": false,
        "error": {
            "code": code,
            "message": message,
        }
    })
}

fn engine_error_response(e: &crate::engine::error::RustQueueError) -> Value {
    json!({
        "ok": false,
        "error": {
            "code": e.error_code(),
            "message": e.to_string(),
        }
    })
}
