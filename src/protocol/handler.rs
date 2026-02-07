//! TCP connection handler — reads newline-delimited JSON commands, dispatches
//! to [`QueueManager`], and writes JSON responses.
//!
//! Supports pipelining: if the client sends multiple commands before waiting
//! for responses, the handler reads all available lines from the buffer,
//! processes them, and writes all responses in a single batch with one flush.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, warn};

use crate::config::AuthConfig;
use crate::engine::models::Schedule;
use crate::engine::queue::{BatchAckItem, BatchPushItem, JobOptions, QueueManager};

/// Handle a single TCP connection, processing commands until the client disconnects.
///
/// The stream type is generic over `AsyncRead + AsyncWrite` so that both plain TCP
/// and TLS connections can be handled with the same logic.
///
/// When `auth_config.enabled` is `true`, the first command on the connection must
/// be `{"cmd":"auth","token":"<bearer-token>"}`. All subsequent commands are allowed
/// only after successful authentication. When auth is disabled, all commands are
/// allowed without an auth handshake.
///
/// ## Pipelining
///
/// After reading the first available line, the handler checks if more data is
/// already buffered (from a client that sent multiple commands without waiting).
/// All buffered lines are read and processed before writing responses, reducing
/// the number of flush syscalls from N to 1 per batch.
pub async fn handle_connection<S>(stream: S, manager: Arc<QueueManager>, auth_config: &AuthConfig)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let peer = "unknown";
    debug!(peer = %peer, "New TCP connection");

    // If auth is disabled, the connection starts as authenticated.
    let mut authenticated = !auth_config.enabled;

    let (reader, writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    let mut writer = tokio::io::BufWriter::new(writer);
    let mut line = String::new();

    loop {
        // 1. Read the first line (blocking wait for data)
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => {
                debug!(peer = %peer, "Client disconnected");
                break;
            }
            Ok(_) => {}
            Err(e) => {
                warn!(peer = %peer, error = %e, "Read error, closing connection");
                break;
            }
        }

        // 2. Collect first line + any additional buffered lines (pipelining)
        let mut lines: Vec<String> = Vec::new();
        lines.push(std::mem::take(&mut line));

        // Drain any already-buffered lines without blocking
        while !reader.buffer().is_empty() {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break, // EOF mid-pipeline
                Ok(_) => lines.push(std::mem::take(&mut line)),
                Err(_) => break,
            }
        }

        // 3. Process all commands and build a single response buffer
        let mut responses = String::new();
        for cmd_line in &lines {
            let response = if authenticated {
                process_line(cmd_line, &manager).await
            } else {
                process_auth_line(cmd_line, auth_config, &mut authenticated)
            };
            let resp_str = serde_json::to_string(&response).unwrap_or_else(|e| {
                json!({"ok": false, "error": {"code": "INTERNAL_ERROR", "message": e.to_string()}})
                    .to_string()
            });
            responses.push_str(&resp_str);
            responses.push('\n');
        }

        // 4. Write all responses in one batch, then flush once
        if writer.write_all(responses.as_bytes()).await.is_err() {
            debug!(peer = %peer, "Write failed, closing connection");
            break;
        }
        if writer.flush().await.is_err() {
            debug!(peer = %peer, "Flush failed, closing connection");
            break;
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
        "push_batch" => handle_push_batch(&parsed, manager).await,
        "pull" => handle_pull(&parsed, manager).await,
        "ack" => handle_ack(&parsed, manager).await,
        "ack_batch" => handle_ack_batch(&parsed, manager).await,
        "fail" => handle_fail(&parsed, manager).await,
        "cancel" => handle_cancel(&parsed, manager).await,
        "progress" => handle_progress(&parsed, manager).await,
        "heartbeat" => handle_heartbeat(&parsed, manager).await,
        "stats" => handle_stats(&parsed, manager).await,
        "schedule_create" => handle_schedule_create(&parsed, manager).await,
        "schedule_list" => handle_schedule_list(manager).await,
        "schedule_get" => handle_schedule_get(&parsed, manager).await,
        "schedule_delete" => handle_schedule_delete(&parsed, manager).await,
        "schedule_pause" => handle_schedule_pause(&parsed, manager).await,
        "schedule_resume" => handle_schedule_resume(&parsed, manager).await,
        "queue_pause" => handle_queue_pause(&parsed, manager),
        "queue_resume" => handle_queue_resume(&parsed, manager),
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

    // Accept options either nested under "options" key or flattened at top level.
    // Try nested first; only attempt expensive top-level parse if common option keys present.
    let options: Option<JobOptions> = cmd
        .get("options")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .or_else(|| {
            if cmd.get("priority").is_some()
                || cmd.get("max_attempts").is_some()
                || cmd.get("delay_ms").is_some()
                || cmd.get("unique_key").is_some()
                || cmd.get("timeout_ms").is_some()
                || cmd.get("ttl_ms").is_some()
                || cmd.get("backoff").is_some()
                || cmd.get("tags").is_some()
            {
                serde_json::from_value(cmd.clone()).ok()
            } else {
                None
            }
        });

    match manager.push(queue, name, data, options).await {
        Ok(id) => json!({"ok": true, "id": id.to_string()}),
        Err(e) => engine_error_response(&e),
    }
}

async fn handle_push_batch(cmd: &Value, manager: &QueueManager) -> Value {
    let queue = match cmd.get("queue").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return error_response("VALIDATION_ERROR", "Missing 'queue' field"),
    };
    let jobs = match cmd.get("jobs").and_then(|v| v.as_array()) {
        Some(jobs) => jobs,
        None => return error_response("VALIDATION_ERROR", "Missing or invalid 'jobs' field"),
    };

    let mut items = Vec::with_capacity(jobs.len());
    for (idx, entry) in jobs.iter().enumerate() {
        let obj = match entry.as_object() {
            Some(obj) => obj,
            None => {
                return error_response(
                    "VALIDATION_ERROR",
                    &format!("jobs[{idx}] must be an object"),
                );
            }
        };

        let name = match obj.get("name").and_then(|v| v.as_str()) {
            Some(name) => name.to_string(),
            None => {
                return error_response(
                    "VALIDATION_ERROR",
                    &format!("jobs[{idx}] missing 'name' field"),
                );
            }
        };
        let data = obj.get("data").cloned().unwrap_or(json!({}));

        // Try nested "options" first; only fallback to top-level if option keys present
        let options: Option<JobOptions> = obj
            .get("options")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .or_else(|| {
                if obj.get("priority").is_some()
                    || obj.get("max_attempts").is_some()
                    || obj.get("delay_ms").is_some()
                    || obj.get("unique_key").is_some()
                {
                    serde_json::from_value(entry.clone()).ok()
                } else {
                    None
                }
            });

        items.push(BatchPushItem {
            name,
            data,
            options,
        });
    }

    match manager.push_batch(queue, items).await {
        Ok(ids) => json!({
            "ok": true,
            "ids": ids.into_iter().map(|id| id.to_string()).collect::<Vec<_>>()
        }),
        Err(e) => engine_error_response(&e),
    }
}

async fn handle_pull(cmd: &Value, manager: &QueueManager) -> Value {
    let queue = match cmd.get("queue").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return error_response("VALIDATION_ERROR", "Missing 'queue' field"),
    };
    let count = cmd.get("count").and_then(|v| v.as_u64()).unwrap_or(1) as u32;

    match manager.pull(queue, count).await {
        Ok(jobs) => {
            if count == 1 {
                // Single-job mode: return the job directly (or null).
                let job_val = jobs
                    .into_iter()
                    .next()
                    .map(|j| json!(j))
                    .unwrap_or(Value::Null);
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

async fn handle_ack_batch(cmd: &Value, manager: &QueueManager) -> Value {
    let mut items: Vec<BatchAckItem> = Vec::new();

    if let Some(entries) = cmd.get("items").and_then(|v| v.as_array()) {
        items.reserve(entries.len());
        for (idx, entry) in entries.iter().enumerate() {
            let obj = match entry.as_object() {
                Some(obj) => obj,
                None => {
                    return error_response(
                        "VALIDATION_ERROR",
                        &format!("items[{idx}] must be an object"),
                    );
                }
            };

            let id_str = match obj.get("id").and_then(|v| v.as_str()) {
                Some(id) => id,
                None => {
                    return error_response(
                        "VALIDATION_ERROR",
                        &format!("items[{idx}] missing 'id' field"),
                    );
                }
            };
            let id = match uuid::Uuid::parse_str(id_str) {
                Ok(id) => id,
                Err(_) => {
                    return error_response(
                        "VALIDATION_ERROR",
                        &format!("items[{idx}] has invalid job ID format"),
                    );
                }
            };
            let result = obj.get("result").cloned();
            items.push(BatchAckItem { id, result });
        }
    } else if let Some(ids) = cmd.get("ids").and_then(|v| v.as_array()) {
        items.reserve(ids.len());
        for (idx, id_value) in ids.iter().enumerate() {
            let id_str = match id_value.as_str() {
                Some(id) => id,
                None => {
                    return error_response(
                        "VALIDATION_ERROR",
                        &format!("ids[{idx}] must be a string"),
                    );
                }
            };
            let id = match uuid::Uuid::parse_str(id_str) {
                Ok(id) => id,
                Err(_) => {
                    return error_response(
                        "VALIDATION_ERROR",
                        &format!("ids[{idx}] has invalid job ID format"),
                    );
                }
            };
            items.push(BatchAckItem { id, result: None });
        }
    } else {
        return error_response(
            "VALIDATION_ERROR",
            "Missing 'items' or 'ids' field for ack_batch",
        );
    }

    match manager.ack_batch(items).await {
        Ok(results) => {
            let acked = results.iter().filter(|r| r.ok).count();
            let failed = results.len().saturating_sub(acked);
            let entries: Vec<Value> = results
                .into_iter()
                .map(|result| {
                    if result.ok {
                        json!({
                            "id": result.id.to_string(),
                            "ok": true
                        })
                    } else {
                        json!({
                            "id": result.id.to_string(),
                            "ok": false,
                            "error": {
                                "code": result.error_code.unwrap_or_else(|| "INTERNAL_ERROR".to_string()),
                                "message": result.error_message.unwrap_or_else(|| "unknown error".to_string()),
                            }
                        })
                    }
                })
                .collect();

            json!({
                "ok": failed == 0,
                "acked": acked,
                "failed": failed,
                "results": entries,
            })
        }
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

// ── Schedule command handlers ────────────────────────────────────────────────

async fn handle_schedule_create(cmd: &Value, manager: &QueueManager) -> Value {
    let name = match cmd.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return error_response("VALIDATION_ERROR", "Missing 'name' field"),
    };
    let queue = match cmd.get("queue").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return error_response("VALIDATION_ERROR", "Missing 'queue' field"),
    };
    let job_name = match cmd.get("job_name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return error_response("VALIDATION_ERROR", "Missing 'job_name' field"),
    };
    let job_data = cmd.get("job_data").cloned().unwrap_or(json!({}));
    let cron_expr = cmd
        .get("cron_expr")
        .and_then(|v| v.as_str())
        .map(String::from);
    let every_ms = cmd.get("every_ms").and_then(|v| v.as_u64());
    let timezone = cmd
        .get("timezone")
        .and_then(|v| v.as_str())
        .map(String::from);
    let max_executions = cmd.get("max_executions").and_then(|v| v.as_u64());
    let job_options: Option<JobOptions> = cmd
        .get("job_options")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    let now = chrono::Utc::now();
    let schedule = Schedule {
        name: name.to_string(),
        queue: queue.to_string(),
        job_name: job_name.to_string(),
        job_data,
        job_options,
        cron_expr,
        every_ms,
        timezone,
        max_executions,
        execution_count: 0,
        paused: false,
        last_run_at: None,
        next_run_at: None,
        created_at: now,
        updated_at: now,
    };

    match manager.create_schedule(&schedule).await {
        Ok(()) => json!({"ok": true}),
        Err(e) => engine_error_response(&e),
    }
}

async fn handle_schedule_list(manager: &QueueManager) -> Value {
    match manager.list_schedules().await {
        Ok(schedules) => {
            let schedules_val: Vec<Value> = schedules.into_iter().map(|s| json!(s)).collect();
            json!({"ok": true, "schedules": schedules_val})
        }
        Err(e) => engine_error_response(&e),
    }
}

async fn handle_schedule_get(cmd: &Value, manager: &QueueManager) -> Value {
    let name = match cmd.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return error_response("VALIDATION_ERROR", "Missing 'name' field"),
    };

    match manager.get_schedule(name).await {
        Ok(Some(schedule)) => json!({"ok": true, "schedule": schedule}),
        Ok(None) => error_response("NOT_FOUND", &format!("Schedule '{name}' not found")),
        Err(e) => engine_error_response(&e),
    }
}

async fn handle_schedule_delete(cmd: &Value, manager: &QueueManager) -> Value {
    let name = match cmd.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return error_response("VALIDATION_ERROR", "Missing 'name' field"),
    };

    match manager.delete_schedule(name).await {
        Ok(()) => json!({"ok": true}),
        Err(e) => engine_error_response(&e),
    }
}

async fn handle_schedule_pause(cmd: &Value, manager: &QueueManager) -> Value {
    let name = match cmd.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return error_response("VALIDATION_ERROR", "Missing 'name' field"),
    };

    match manager.pause_schedule(name).await {
        Ok(()) => json!({"ok": true}),
        Err(e) => engine_error_response(&e),
    }
}

async fn handle_schedule_resume(cmd: &Value, manager: &QueueManager) -> Value {
    let name = match cmd.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return error_response("VALIDATION_ERROR", "Missing 'name' field"),
    };

    match manager.resume_schedule(name).await {
        Ok(()) => json!({"ok": true}),
        Err(e) => engine_error_response(&e),
    }
}

// ── Queue pause/resume handlers ──────────────────────────────────────────────

fn handle_queue_pause(cmd: &Value, manager: &QueueManager) -> Value {
    let queue = match cmd.get("queue").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return error_response("VALIDATION_ERROR", "Missing 'queue' field"),
    };
    manager.pause_queue(queue);
    json!({"ok": true})
}

fn handle_queue_resume(cmd: &Value, manager: &QueueManager) -> Value {
    let queue = match cmd.get("queue").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return error_response("VALIDATION_ERROR", "Missing 'queue' field"),
    };
    manager.resume_queue(queue);
    json!({"ok": true})
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
