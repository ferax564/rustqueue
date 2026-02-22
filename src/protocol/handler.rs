//! TCP connection handler — reads newline-delimited JSON commands, dispatches
//! to [`QueueManager`], and writes JSON responses.
//!
//! Also supports the binary wire protocol defined in [`super::binary`]. The
//! handler auto-detects the frame type by peeking at the first byte:
//! - `0x01`–`0x03`: binary command (PushBatch, PullBatch, AckBatch)
//! - Any other byte: newline-delimited JSON
//!
//! Supports pipelining: if the client sends multiple commands before waiting
//! for responses, the handler reads all available lines from the buffer,
//! processes them, and writes all responses in a single batch with one flush.

use std::sync::Arc;

use bytes::{Bytes, BytesMut};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, warn};

use crate::auth::validate_bearer_token;
use crate::config::AuthConfig;
use crate::engine::models::Schedule;
use crate::engine::queue::{BatchAckItem, BatchPushItem, JobOptions, QueueManager};

use super::binary::{self, BinaryCommand};

/// Initial capacity for the binary frame receive buffer (64 KB).
const BINARY_RECV_BUF_CAPACITY: usize = 64 * 1024;

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
        // Peek the first byte to determine frame type (binary vs JSON).
        let first_byte = match reader.fill_buf().await {
            Ok([]) => {
                debug!(peer = %peer, "Client disconnected");
                break;
            }
            Ok(buf) => buf[0],
            Err(e) => {
                warn!(peer = %peer, error = %e, "Read error, closing connection");
                break;
            }
        };

        // Binary frame detection: command bytes 0x01–0x03 and 0x10 never appear
        // as the first byte of valid JSON (which starts with `{`, `[`, `"`, or digits).
        if matches!(
            BinaryCommand::try_from(first_byte),
            Ok(BinaryCommand::PushBatch | BinaryCommand::PullBatch | BinaryCommand::AckBatch | BinaryCommand::ChannelFrame)
        ) {
            if !authenticated {
                // Binary protocol requires pre-authentication on this connection.
                let err = binary::encode_error_response(
                    "Authentication required before binary commands",
                );
                if writer.write_all(&err).await.is_err() {
                    break;
                }
                let _ = writer.flush().await;
                break;
            }

            match read_and_handle_binary_frame(&mut reader, &mut writer, &manager).await {
                Ok(true) => continue,  // frame handled, continue loop
                Ok(false) => break,    // write error, close connection
                Err(_) => break,       // read/decode error, close connection
            }
        }

        // JSON path: read a newline-delimited line.
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

// ── Binary protocol handling ─────────────────────────────────────────────────

/// Maximum binary frame size (4 MB). Protects against memory exhaustion from
/// malicious or malformed frames.
const MAX_BINARY_FRAME_SIZE: usize = 4 * 1_048_576;

/// Read a complete binary frame from the buffered reader, dispatch it, and
/// write the binary response.
///
/// Returns `Ok(true)` if the frame was handled and the connection should
/// continue, `Ok(false)` if a write error occurred, or `Err` if a read or
/// decode error occurred (which closes the connection).
async fn read_and_handle_binary_frame<R, W>(
    reader: &mut BufReader<R>,
    writer: &mut tokio::io::BufWriter<W>,
    manager: &QueueManager,
) -> Result<bool, ()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    // Read the command byte (already peeked, now consume it).
    let mut cmd_byte = [0u8; 1];
    if reader.read_exact(&mut cmd_byte).await.is_err() {
        return Err(());
    }

    let cmd = match BinaryCommand::try_from(cmd_byte[0]) {
        Ok(cmd) => cmd,
        Err(_) => {
            let err = binary::encode_error_response("Invalid binary command");
            if writer.write_all(&err).await.is_err() {
                return Ok(false);
            }
            let _ = writer.flush().await;
            return Err(());
        }
    };

    // Channel frame: read channel_id then recurse for the inner command.
    if cmd == BinaryCommand::ChannelFrame {
        // Read 2-byte channel_id
        let mut chan_buf = [0u8; 2];
        if reader.read_exact(&mut chan_buf).await.is_err() {
            return Err(());
        }
        let channel_id = u16::from_be_bytes(chan_buf);

        // Read the inner command byte
        let mut inner_cmd_byte = [0u8; 1];
        if reader.read_exact(&mut inner_cmd_byte).await.is_err() {
            return Err(());
        }
        let inner_cmd = match BinaryCommand::try_from(inner_cmd_byte[0]) {
            Ok(c) => c,
            Err(_) => {
                let err = binary::encode_channel_response(
                    channel_id,
                    &binary::encode_error_response("Invalid inner command in channel frame"),
                );
                if writer.write_all(&err).await.is_err() {
                    return Ok(false);
                }
                let _ = writer.flush().await;
                return Err(());
            }
        };

        // Read the inner frame body
        let inner_data = match read_binary_frame_body(reader, inner_cmd).await {
            Ok(data) => data,
            Err(msg) => {
                let err = binary::encode_channel_response(
                    channel_id,
                    &binary::encode_error_response(&msg),
                );
                if writer.write_all(&err).await.is_err() {
                    return Ok(false);
                }
                let _ = writer.flush().await;
                return Err(());
            }
        };

        // Build the inner frame and process it
        let mut inner_frame = BytesMut::with_capacity(1 + inner_data.len());
        inner_frame.extend_from_slice(&inner_cmd_byte);
        inner_frame.extend_from_slice(&inner_data);
        let inner_bytes = inner_frame.freeze();

        let inner_response = handle_binary_frame_zero_copy(inner_bytes, manager).await;

        // Wrap response with channel_id
        let response = binary::encode_channel_response(channel_id, &inner_response);
        if writer.write_all(&response).await.is_err() {
            return Ok(false);
        }
        if writer.flush().await.is_err() {
            return Ok(false);
        }
        return Ok(true);
    }

    // Read the rest of the frame based on command type.
    let frame_data = match read_binary_frame_body(reader, cmd).await {
        Ok(data) => data,
        Err(msg) => {
            let err = binary::encode_error_response(&msg);
            if writer.write_all(&err).await.is_err() {
                return Ok(false);
            }
            let _ = writer.flush().await;
            return Err(());
        }
    };

    // Prepend the command byte to form the full frame for decoding.
    // Use BytesMut for efficient buffer construction, then freeze into
    // Bytes for zero-copy payload slicing in decode.
    let mut full_frame = BytesMut::with_capacity(
        (1 + frame_data.len()).max(BINARY_RECV_BUF_CAPACITY),
    );
    full_frame.extend_from_slice(&cmd_byte);
    full_frame.extend_from_slice(&frame_data);
    let frame_bytes = full_frame.freeze();

    let response = handle_binary_frame_zero_copy(frame_bytes, manager).await;

    if writer.write_all(&response).await.is_err() {
        return Ok(false);
    }
    if writer.flush().await.is_err() {
        return Ok(false);
    }

    Ok(true)
}

/// Read the body of a binary frame (everything after the command byte).
///
/// For PushBatch: reads queue name length, queue name, batch count, then
/// length-prefixed payloads.
/// For PullBatch: reads queue name length, queue name, count (4 bytes).
/// For AckBatch: reads count (4 bytes), then count * 16 bytes of UUIDs.
async fn read_binary_frame_body<R>(
    reader: &mut BufReader<R>,
    cmd: BinaryCommand,
) -> Result<Vec<u8>, String>
where
    R: tokio::io::AsyncRead + Unpin,
{
    match cmd {
        BinaryCommand::PushBatch => {
            // Read queue name length (2 bytes)
            let mut header = [0u8; 2];
            reader
                .read_exact(&mut header)
                .await
                .map_err(|e| format!("Failed to read queue name length: {e}"))?;
            let queue_len = u16::from_be_bytes(header) as usize;

            // Read queue name
            let mut queue_name = vec![0u8; queue_len];
            reader
                .read_exact(&mut queue_name)
                .await
                .map_err(|e| format!("Failed to read queue name: {e}"))?;

            // Read batch count (4 bytes)
            let mut count_buf = [0u8; 4];
            reader
                .read_exact(&mut count_buf)
                .await
                .map_err(|e| format!("Failed to read batch count: {e}"))?;
            let count = u32::from_be_bytes(count_buf) as usize;

            // Pre-allocate body: 2 (queue_len) + queue_len + 4 (count) + payloads
            let mut body = Vec::with_capacity(2 + queue_len + 4);
            body.extend_from_slice(&header);
            body.extend_from_slice(&queue_name);
            body.extend_from_slice(&count_buf);

            // Read each length-prefixed payload
            for _ in 0..count {
                let mut len_buf = [0u8; 4];
                reader
                    .read_exact(&mut len_buf)
                    .await
                    .map_err(|e| format!("Failed to read payload length: {e}"))?;
                let msg_len = u32::from_be_bytes(len_buf) as usize;
                if msg_len > MAX_BINARY_FRAME_SIZE {
                    return Err(format!("Payload too large: {msg_len} bytes"));
                }
                let mut payload = vec![0u8; msg_len];
                reader
                    .read_exact(&mut payload)
                    .await
                    .map_err(|e| format!("Failed to read payload data: {e}"))?;
                body.extend_from_slice(&len_buf);
                body.extend_from_slice(&payload);
            }

            Ok(body)
        }
        BinaryCommand::PullBatch => {
            // Read queue name length (2 bytes) + queue name + count (4 bytes)
            let mut header = [0u8; 2];
            reader
                .read_exact(&mut header)
                .await
                .map_err(|e| format!("Failed to read queue name length: {e}"))?;
            let queue_len = u16::from_be_bytes(header) as usize;

            let mut queue_name = vec![0u8; queue_len];
            reader
                .read_exact(&mut queue_name)
                .await
                .map_err(|e| format!("Failed to read queue name: {e}"))?;

            let mut count_buf = [0u8; 4];
            reader
                .read_exact(&mut count_buf)
                .await
                .map_err(|e| format!("Failed to read count: {e}"))?;

            let mut body = Vec::with_capacity(2 + queue_len + 4);
            body.extend_from_slice(&header);
            body.extend_from_slice(&queue_name);
            body.extend_from_slice(&count_buf);

            Ok(body)
        }
        BinaryCommand::AckBatch => {
            // Read count (4 bytes) + count * 16 bytes of UUIDs
            let mut count_buf = [0u8; 4];
            reader
                .read_exact(&mut count_buf)
                .await
                .map_err(|e| format!("Failed to read count: {e}"))?;
            let count = u32::from_be_bytes(count_buf) as usize;

            let uuid_data_len = count * 16;
            if uuid_data_len > MAX_BINARY_FRAME_SIZE {
                return Err(format!("Ack batch too large: {count} items"));
            }
            let mut uuid_data = vec![0u8; uuid_data_len];
            reader
                .read_exact(&mut uuid_data)
                .await
                .map_err(|e| format!("Failed to read UUID data: {e}"))?;

            let mut body = Vec::with_capacity(4 + uuid_data_len);
            body.extend_from_slice(&count_buf);
            body.extend_from_slice(&uuid_data);

            Ok(body)
        }
        // ChannelFrame is handled before this function is called.
        BinaryCommand::ChannelFrame => {
            Err("ChannelFrame should be handled before read_binary_frame_body".to_string())
        }
    }
}

/// Handle a complete binary frame and return the binary response.
///
/// This function decodes the binary command, executes it against the
/// [`QueueManager`], and encodes the binary response. It can be called
/// independently for testing.
///
/// # Arguments
///
/// * `data` — The full binary frame including the command byte.
/// * `manager` — The queue manager to execute commands against.
///
/// # Returns
///
/// A binary response suitable for writing directly to the TCP stream.
pub async fn handle_binary_frame(data: &[u8], manager: &QueueManager) -> Vec<u8> {
    if data.is_empty() {
        return binary::encode_error_response("Empty binary frame");
    }

    let cmd = match BinaryCommand::try_from(data[0]) {
        Ok(cmd) => cmd,
        Err(_) => {
            return binary::encode_error_response(&format!("Invalid command byte: {:#04x}", data[0]));
        }
    };

    match cmd {
        BinaryCommand::PushBatch => handle_binary_push_batch(data, manager).await,
        BinaryCommand::PullBatch => handle_binary_pull_batch(data, manager).await,
        BinaryCommand::AckBatch => handle_binary_ack_batch(data, manager).await,
        BinaryCommand::ChannelFrame => {
            // Unwrap channel frame and process inner command
            match binary::decode_channel_frame(data) {
                Ok((channel_id, inner_data)) => {
                    let inner_response = Box::pin(handle_binary_frame(inner_data, manager)).await;
                    binary::encode_channel_response(channel_id, &inner_response)
                }
                Err(e) => binary::encode_error_response(&format!("Channel frame error: {e}")),
            }
        }
    }
}

/// Zero-copy variant of [`handle_binary_frame`] that takes ownership of a
/// `Bytes` buffer. For PushBatch commands, payloads are sliced from the
/// original buffer without copying.
async fn handle_binary_frame_zero_copy(data: Bytes, manager: &QueueManager) -> Vec<u8> {
    if data.is_empty() {
        return binary::encode_error_response("Empty binary frame");
    }

    let cmd = match BinaryCommand::try_from(data[0]) {
        Ok(cmd) => cmd,
        Err(_) => {
            return binary::encode_error_response(&format!("Invalid command byte: {:#04x}", data[0]));
        }
    };

    match cmd {
        BinaryCommand::PushBatch => handle_binary_push_batch_zero_copy(data, manager).await,
        BinaryCommand::PullBatch => handle_binary_pull_batch(&data, manager).await,
        BinaryCommand::AckBatch => handle_binary_ack_batch(&data, manager).await,
        BinaryCommand::ChannelFrame => {
            // Unwrap channel frame and process inner command
            match binary::decode_channel_frame(&data) {
                Ok((channel_id, inner_data)) => {
                    let inner_response = Box::pin(handle_binary_frame(inner_data, manager)).await;
                    binary::encode_channel_response(channel_id, &inner_response)
                }
                Err(e) => binary::encode_error_response(&format!("Channel frame error: {e}")),
            }
        }
    }
}

/// Prefix for auto-generated job names in the binary protocol.
const BINARY_JOB_PREFIX: &str = "binary-job-";

/// Convert raw binary payloads into [`BatchPushItem`]s suitable for
/// [`QueueManager::push_batch`].
///
/// Optimizations over the naive approach:
/// 1. **Fast-path JSON detection:** Checks first byte for `{` or `[` before
///    attempting `serde_json::from_slice`, avoiding the full parse attempt on
///    payloads that are clearly not JSON.
/// 2. **UTF-8 lossy fallback instead of hex encoding:** Non-JSON payloads are
///    stored as `Value::String` via `String::from_utf8_lossy`, which is
///    significantly faster than hex-encoding (no 2x expansion, no per-byte
///    formatting). The storage layer serializes this back to JSON as a string.
/// 3. **Pre-allocated job name:** Uses a single `String` buffer with
///    `truncate` + `push_str` to avoid per-item `format!` allocations.
fn build_batch_push_items<'a>(payloads: impl ExactSizeIterator<Item = &'a [u8]>) -> Vec<BatchPushItem> {
    let len = payloads.len();
    let mut items = Vec::with_capacity(len);
    // Pre-allocate a name buffer: "binary-job-" + up to 10 digits for u32 index.
    let mut name_buf = String::with_capacity(BINARY_JOB_PREFIX.len() + 10);

    for (idx, payload) in payloads.enumerate() {
        // Build the job name by reusing the buffer.
        name_buf.clear();
        name_buf.push_str(BINARY_JOB_PREFIX);
        // itoa is faster than format! for integer-to-string, but we can use
        // the Display impl which is close enough and avoids a new dependency.
        use std::fmt::Write;
        let _ = write!(name_buf, "{idx}");

        // Fast-path: only attempt JSON parsing if the payload starts with
        // `{` or `[` (the two valid JSON value opening bytes for objects/arrays).
        // Scalar JSON values (strings, numbers, booleans, null) are uncommon
        // as job payloads and will be caught by the fallback path.
        let data_value = if !payload.is_empty() && (payload[0] == b'{' || payload[0] == b'[') {
            serde_json::from_slice(payload).unwrap_or_else(|_| {
                Value::String(String::from_utf8_lossy(payload).into_owned())
            })
        } else {
            // Non-JSON payload: store as a string value. This is faster than
            // hex encoding (which doubles the size) and produces more readable
            // output for text-like payloads.
            match serde_json::from_slice(payload) {
                Ok(v) => v,
                Err(_) => Value::String(String::from_utf8_lossy(payload).into_owned()),
            }
        };

        items.push(BatchPushItem {
            name: name_buf.clone(),
            data: data_value,
            options: None,
        });
    }

    items
}

/// Handle a binary PushBatch command.
async fn handle_binary_push_batch(data: &[u8], manager: &QueueManager) -> Vec<u8> {
    let (queue_name, payloads) = match binary::decode_push_batch(data) {
        Ok(result) => result,
        Err(e) => {
            return binary::encode_error_response(&format!("Decode error: {e}"));
        }
    };

    let items = build_batch_push_items(payloads.iter().map(|p| p.as_slice()));

    match manager.push_batch(&queue_name, items).await {
        Ok(ids) => binary::encode_push_response(&ids),
        Err(e) => binary::encode_error_response(&e.to_string()),
    }
}

/// Zero-copy variant of push batch handler. Payloads are `Bytes` slices
/// into the original frame buffer, avoiding per-payload memcpy during frame
/// decoding. Note that `serde_json::from_slice` still allocates when parsing
/// the JSON into a `Value` tree — the zero-copy benefit is in the frame
/// splitting stage (one refcounted `Bytes` buffer instead of N `Vec<u8>`
/// allocations).
async fn handle_binary_push_batch_zero_copy(data: Bytes, manager: &QueueManager) -> Vec<u8> {
    let (queue_name, payloads) = match binary::decode_push_batch_zero_copy(data) {
        Ok(result) => result,
        Err(e) => {
            return binary::encode_error_response(&format!("Decode error: {e}"));
        }
    };

    let items = build_batch_push_items(payloads.iter().map(|p| p.as_ref()));

    match manager.push_batch(&queue_name, items).await {
        Ok(ids) => binary::encode_push_response(&ids),
        Err(e) => binary::encode_error_response(&e.to_string()),
    }
}

/// Handle a binary PullBatch command.
async fn handle_binary_pull_batch(data: &[u8], manager: &QueueManager) -> Vec<u8> {
    let (queue_name, count) = match binary::decode_pull_batch(data) {
        Ok(result) => result,
        Err(e) => {
            return binary::encode_error_response(&format!("Decode error: {e}"));
        }
    };

    match manager.pull(&queue_name, count).await {
        Ok(jobs) => {
            let serialized: Vec<(uuid::Uuid, Vec<u8>)> = jobs
                .iter()
                .map(|job| {
                    let payload = serde_json::to_vec(&job.data).unwrap_or_default();
                    (job.id, payload)
                })
                .collect();
            let refs: Vec<(uuid::Uuid, &[u8])> = serialized
                .iter()
                .map(|(id, p)| (*id, p.as_slice()))
                .collect();
            binary::encode_pull_response(&refs)
        }
        Err(e) => binary::encode_error_response(&e.to_string()),
    }
}

/// Handle a binary AckBatch command.
async fn handle_binary_ack_batch(data: &[u8], manager: &QueueManager) -> Vec<u8> {
    let job_ids = match binary::decode_ack_batch(data) {
        Ok(ids) => ids,
        Err(e) => {
            return binary::encode_error_response(&format!("Decode error: {e}"));
        }
    };

    let items: Vec<BatchAckItem> = job_ids
        .into_iter()
        .map(|id| BatchAckItem { id, result: None })
        .collect();

    match manager.ack_batch(items).await {
        Ok(results) => {
            let acked = results.iter().filter(|r| r.ok).count() as u32;
            let failed = (results.len() as u32).saturating_sub(acked);
            binary::encode_ack_response(acked, failed)
        }
        Err(e) => binary::encode_error_response(&e.to_string()),
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

    if validate_bearer_token(token, &auth_config.tokens).is_ok() {
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
                || cmd.get("depends_on").is_some()
                || cmd.get("flow_id").is_some()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    fn make_manager() -> Arc<QueueManager> {
        Arc::new(QueueManager::new(Arc::new(MemoryStorage::new())))
    }

    #[tokio::test]
    async fn binary_push_batch_handler_roundtrip() {
        let manager = make_manager();
        let payloads: Vec<&[u8]> = vec![br#"{"a":1}"#, br#"{"b":2}"#];
        let frame = binary::encode_push_batch("test-q", &payloads);

        let response = handle_binary_frame(&frame, &manager).await;

        // Decode response: status=0x00, count=2, 2 * 16 bytes UUID
        assert_eq!(response[0], 0x00, "Expected success status");
        let ids = binary::decode_push_response(&response).unwrap();
        assert_eq!(ids.len(), 2, "Should return 2 job IDs");
        assert!(ids.iter().all(|id| !id.is_nil()));
    }

    #[tokio::test]
    async fn binary_push_then_pull_handler() {
        let manager = make_manager();

        // Push 1 job
        let payloads: Vec<&[u8]> = vec![br#"{"key":"value"}"#];
        let push_frame = binary::encode_push_batch("pull-test", &payloads);
        let push_resp = handle_binary_frame(&push_frame, &manager).await;
        let pushed_ids = binary::decode_push_response(&push_resp).unwrap();
        assert_eq!(pushed_ids.len(), 1);

        // Pull 1 job
        let pull_frame = binary::encode_pull_batch("pull-test", 1);
        let pull_resp = handle_binary_frame(&pull_frame, &manager).await;
        assert_eq!(pull_resp[0], 0x00, "pull should succeed");
        let count = u32::from_be_bytes([pull_resp[1], pull_resp[2], pull_resp[3], pull_resp[4]]) as usize;
        assert_eq!(count, 1, "should pull 1 job");
    }

    #[tokio::test]
    async fn binary_ack_batch_handler() {
        let manager = make_manager();

        // Push 2 jobs
        let payloads: Vec<&[u8]> = vec![br#"{}"#, br#"{}"#];
        let push_frame = binary::encode_push_batch("ack-test", &payloads);
        let push_resp = handle_binary_frame(&push_frame, &manager).await;
        let pushed_ids = binary::decode_push_response(&push_resp).unwrap();

        // Pull them (transitions to Active state so they can be acked)
        let pull_frame = binary::encode_pull_batch("ack-test", 2);
        let _pull_resp = handle_binary_frame(&pull_frame, &manager).await;

        // Ack both
        let ack_frame = binary::encode_ack_batch(&pushed_ids);
        let ack_resp = handle_binary_frame(&ack_frame, &manager).await;

        assert_eq!(ack_resp[0], 0x00, "ack should succeed");
        let acked = u32::from_be_bytes([ack_resp[1], ack_resp[2], ack_resp[3], ack_resp[4]]);
        let failed = u32::from_be_bytes([ack_resp[5], ack_resp[6], ack_resp[7], ack_resp[8]]);
        assert_eq!(acked, 2);
        assert_eq!(failed, 0);
    }

    #[tokio::test]
    async fn zero_copy_push_batch_handler() {
        let manager = make_manager();
        let payloads: Vec<&[u8]> = vec![br#"{"x":1}"#, br#"{"y":2}"#, br#"{"z":3}"#];
        let frame = binary::encode_push_batch("zc-q", &payloads);
        let frame_bytes = Bytes::from(frame);

        let response = handle_binary_frame_zero_copy(frame_bytes, &manager).await;

        assert_eq!(response[0], 0x00, "Expected success status");
        let ids = binary::decode_push_response(&response).unwrap();
        assert_eq!(ids.len(), 3, "Should return 3 job IDs");
        assert!(ids.iter().all(|id| !id.is_nil()));
    }

    #[tokio::test]
    async fn binary_invalid_command_byte() {
        let manager = make_manager();
        let response = handle_binary_frame(&[0xFF], &manager).await;
        // Should be an error response
        assert_eq!(response[0], 0x01, "Expected error status");
    }

    #[tokio::test]
    async fn binary_empty_frame() {
        let manager = make_manager();
        let response = handle_binary_frame(&[], &manager).await;
        assert_eq!(response[0], 0x01, "Expected error status for empty frame");
    }

    #[tokio::test]
    async fn channel_frame_push_batch() {
        let manager = make_manager();
        let payloads: Vec<&[u8]> = vec![br#"{"ch":1}"#];
        let inner_frame = binary::encode_push_batch("chan-q", &payloads);
        let channel_frame = binary::encode_channel_frame(42, &inner_frame);

        let response = handle_binary_frame(&channel_frame, &manager).await;

        // Response should be prefixed with channel_id=42
        let channel_id = u16::from_be_bytes([response[0], response[1]]);
        assert_eq!(channel_id, 42, "Response should carry channel_id=42");

        // Inner response starts at offset 2
        let inner_resp = &response[2..];
        assert_eq!(inner_resp[0], 0x00, "Inner response should be success");
        let ids = binary::decode_push_response(inner_resp).unwrap();
        assert_eq!(ids.len(), 1);
    }

    #[tokio::test]
    async fn channel_frame_different_channels_same_connection() {
        let manager = make_manager();

        // Push on channel 1
        let push1 = binary::encode_push_batch("q1", &[br#"{"a":1}"#]);
        let chan1 = binary::encode_channel_frame(1, &push1);
        let resp1 = handle_binary_frame(&chan1, &manager).await;
        assert_eq!(u16::from_be_bytes([resp1[0], resp1[1]]), 1);

        // Push on channel 2 (different queue)
        let push2 = binary::encode_push_batch("q2", &[br#"{"b":2}"#]);
        let chan2 = binary::encode_channel_frame(2, &push2);
        let resp2 = handle_binary_frame(&chan2, &manager).await;
        assert_eq!(u16::from_be_bytes([resp2[0], resp2[1]]), 2);

        // Both should succeed
        assert_eq!(resp1[2], 0x00);
        assert_eq!(resp2[2], 0x00);
    }

    #[tokio::test]
    async fn channel_frame_zero_is_default() {
        let manager = make_manager();
        let payloads: Vec<&[u8]> = vec![br#"{}"#];
        let inner_frame = binary::encode_push_batch("default-q", &payloads);
        let channel_frame = binary::encode_channel_frame(0, &inner_frame);

        let response = handle_binary_frame(&channel_frame, &manager).await;
        let channel_id = u16::from_be_bytes([response[0], response[1]]);
        assert_eq!(channel_id, 0, "Default channel should be 0");
        assert_eq!(response[2], 0x00, "Should succeed");
    }

    #[tokio::test]
    async fn backward_compat_raw_command_no_channel() {
        // Existing clients send raw 0x01-0x03 without channel wrapper.
        // This should still work (treated as channel 0 implicitly).
        let manager = make_manager();
        let payloads: Vec<&[u8]> = vec![br#"{"legacy":true}"#];
        let frame = binary::encode_push_batch("legacy-q", &payloads);
        let response = handle_binary_frame(&frame, &manager).await;

        // No channel prefix, direct response
        assert_eq!(response[0], 0x00, "Raw command should succeed");
        let ids = binary::decode_push_response(&response).unwrap();
        assert_eq!(ids.len(), 1);
    }

    #[tokio::test]
    async fn zero_copy_handler_json_and_binary_payloads() {
        // Verify that the zero-copy handler correctly processes a mix of
        // valid JSON payloads and raw binary (non-JSON) payloads.
        let manager = make_manager();

        // Mix of: JSON object, JSON array, plain text (not JSON), raw bytes
        let json_obj = br#"{"key":"value"}"#;
        let json_arr = br#"[1,2,3]"#;
        let plain_text = b"hello world";
        let raw_bytes: &[u8] = &[0x00, 0xFF, 0xAB, 0xCD];

        let payloads: Vec<&[u8]> = vec![json_obj, json_arr, plain_text, raw_bytes];
        let frame = binary::encode_push_batch("mixed-q", &payloads);
        let frame_bytes = Bytes::from(frame);

        let response = handle_binary_frame_zero_copy(frame_bytes, &manager).await;
        assert_eq!(response[0], 0x00, "Expected success status");
        let ids = binary::decode_push_response(&response).unwrap();
        assert_eq!(ids.len(), 4, "Should return 4 job IDs");

        // Pull all jobs back and verify data is preserved correctly.
        let pull_frame = binary::encode_pull_batch("mixed-q", 4);
        let pull_resp = handle_binary_frame(&pull_frame, &manager).await;
        assert_eq!(pull_resp[0], 0x00, "Pull should succeed");
        let count =
            u32::from_be_bytes([pull_resp[1], pull_resp[2], pull_resp[3], pull_resp[4]]) as usize;
        assert_eq!(count, 4, "Should pull 4 jobs");
    }

    #[tokio::test]
    async fn zero_copy_handler_non_json_uses_lossy_string() {
        // Verify that non-JSON payloads are stored as string values
        // (via from_utf8_lossy) rather than hex-encoded.
        let manager = make_manager();

        let payloads: Vec<&[u8]> = vec![b"plain-text-payload"];
        let frame = binary::encode_push_batch("lossy-q", &payloads);
        let frame_bytes = Bytes::from(frame);

        let response = handle_binary_frame_zero_copy(frame_bytes, &manager).await;
        assert_eq!(response[0], 0x00, "Expected success status");

        // Pull the job to inspect its data
        let jobs = manager.pull("lossy-q", 1).await.unwrap();
        assert_eq!(jobs.len(), 1);
        // The data should be a JSON string containing "plain-text-payload"
        // (not hex-encoded like "706c61696e2d746578742d7061796c6f6164")
        assert_eq!(jobs[0].data, json!("plain-text-payload"));
    }

    #[test]
    fn build_batch_push_items_name_format() {
        // Verify that job names are correctly formatted.
        let payloads: Vec<&[u8]> = vec![br#"{}"#, br#"{}"#, br#"{}"#];
        let items = build_batch_push_items(payloads.iter().map(|p| *p as &[u8]));
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].name, "binary-job-0");
        assert_eq!(items[1].name, "binary-job-1");
        assert_eq!(items[2].name, "binary-job-2");
    }

    #[test]
    fn build_batch_push_items_json_parsing() {
        // JSON objects/arrays should be parsed into their Value tree.
        let payloads: Vec<&[u8]> = vec![
            br#"{"a":1}"#,
            br#"[1,2]"#,
            b"not-json",
            &[0xFF, 0xFE],
        ];
        let items = build_batch_push_items(payloads.iter().map(|p| *p as &[u8]));
        assert_eq!(items.len(), 4);

        // JSON object preserved
        assert_eq!(items[0].data, json!({"a": 1}));
        // JSON array preserved
        assert_eq!(items[1].data, json!([1, 2]));
        // Plain text stored as string
        assert_eq!(items[2].data, json!("not-json"));
        // Raw bytes stored as lossy UTF-8 string (with replacement chars)
        assert!(items[3].data.is_string());
    }
}
