//! Binary wire protocol for high-throughput batch message passing.
//!
//! Provides a compact binary frame format for push/pull/ack batch operations,
//! avoiding JSON serialization overhead for bulk workloads.
//!
//! Uses `bytes::Bytes` for zero-copy payload references where possible,
//! avoiding memcpy for large payloads.
//!
//! # Wire Format
//!
//! ## Push Batch Request
//!
//! ```text
//! [1 byte:  command_id]      0x01 = push_batch
//! [2 bytes: queue_name_len]  BE u16
//! [N bytes: queue_name]      UTF-8
//! [4 bytes: batch_count]     BE u32
//! For each message:
//!   [4 bytes: msg_len]       BE u32
//!   [N bytes: msg_data]      raw bytes
//! ```
//!
//! ## Response
//!
//! ```text
//! [1 byte:  status]          0x00 = ok, 0x01 = error
//! [4 bytes: count]           BE u32
//! For each item:
//!   [16 bytes: job_id]       UUID bytes
//! ```

use bytes::Bytes;

/// Maximum single payload size (1 MB).
const MAX_PAYLOAD_SIZE: usize = 1_048_576;

/// Binary command identifiers.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryCommand {
    PushBatch = 0x01,
    PullBatch = 0x02,
    AckBatch = 0x03,
    /// Channel-multiplexed frame wrapper. Wire format:
    /// `[0x10][channel_id:2 BE u16][inner_command_byte][inner_payload...]`
    ///
    /// Enables multiple logical channels over a single TCP connection,
    /// each operating independently on different queues.
    /// Clients that don't use channels send raw 0x01-0x03 commands and
    /// are treated as channel 0 (backward compatible).
    ChannelFrame = 0x10,
}

impl TryFrom<u8> for BinaryCommand {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::PushBatch),
            0x02 => Ok(Self::PullBatch),
            0x03 => Ok(Self::AckBatch),
            0x10 => Ok(Self::ChannelFrame),
            other => Err(ProtocolError::InvalidCommand(other)),
        }
    }
}

/// Errors that can occur during binary protocol encode/decode.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("insufficient data: need {need} bytes, have {have}")]
    InsufficientData { need: usize, have: usize },
    #[error("invalid command id: {0}")]
    InvalidCommand(u8),
    #[error("invalid UTF-8 in queue name")]
    InvalidUtf8,
    #[error("payload too large: {size} bytes")]
    PayloadTooLarge { size: usize },
}

/// Encode a push-batch request into the binary wire format.
///
/// Layout:
/// - 1 byte command (0x01)
/// - 2 bytes queue name length (BE u16)
/// - N bytes queue name (UTF-8)
/// - 4 bytes batch count (BE u32)
/// - For each payload: 4 bytes length (BE u32) + N bytes data
pub fn encode_push_batch(queue: &str, payloads: &[&[u8]]) -> Vec<u8> {
    let queue_bytes = queue.as_bytes();
    // Pre-calculate total size for a single allocation.
    let payload_data_size: usize = payloads.iter().map(|p| 4 + p.len()).sum();
    let total = 1 + 2 + queue_bytes.len() + 4 + payload_data_size;

    let mut buf = Vec::with_capacity(total);
    buf.push(BinaryCommand::PushBatch as u8);
    buf.extend_from_slice(&(queue_bytes.len() as u16).to_be_bytes());
    buf.extend_from_slice(queue_bytes);
    buf.extend_from_slice(&(payloads.len() as u32).to_be_bytes());
    for payload in payloads {
        buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        buf.extend_from_slice(payload);
    }
    buf
}

/// Decode a push-batch request from the binary wire format.
///
/// Returns `(queue_name, payloads)` on success.
pub fn decode_push_batch(data: &[u8]) -> Result<(String, Vec<Vec<u8>>), ProtocolError> {
    let mut pos = 0;

    // Command byte
    if data.is_empty() {
        return Err(ProtocolError::InsufficientData {
            need: 1,
            have: 0,
        });
    }
    let cmd = BinaryCommand::try_from(data[pos])?;
    if cmd != BinaryCommand::PushBatch {
        return Err(ProtocolError::InvalidCommand(data[pos]));
    }
    pos += 1;

    // Queue name length
    if data.len() < pos + 2 {
        return Err(ProtocolError::InsufficientData {
            need: pos + 2,
            have: data.len(),
        });
    }
    let queue_len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
    pos += 2;

    // Queue name
    if data.len() < pos + queue_len {
        return Err(ProtocolError::InsufficientData {
            need: pos + queue_len,
            have: data.len(),
        });
    }
    let queue = std::str::from_utf8(&data[pos..pos + queue_len])
        .map_err(|_| ProtocolError::InvalidUtf8)?
        .to_string();
    pos += queue_len;

    // Batch count
    if data.len() < pos + 4 {
        return Err(ProtocolError::InsufficientData {
            need: pos + 4,
            have: data.len(),
        });
    }
    let count = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
    pos += 4;

    // Payloads
    let mut payloads = Vec::with_capacity(count);
    for _ in 0..count {
        if data.len() < pos + 4 {
            return Err(ProtocolError::InsufficientData {
                need: pos + 4,
                have: data.len(),
            });
        }
        let msg_len =
            u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 4;

        if msg_len > MAX_PAYLOAD_SIZE {
            return Err(ProtocolError::PayloadTooLarge { size: msg_len });
        }

        if data.len() < pos + msg_len {
            return Err(ProtocolError::InsufficientData {
                need: pos + msg_len,
                have: data.len(),
            });
        }
        payloads.push(data[pos..pos + msg_len].to_vec());
        pos += msg_len;
    }

    Ok((queue, payloads))
}

/// Zero-copy decode of a push-batch request from `Bytes`.
///
/// Returns `(queue_name, payloads)` where each payload is a `Bytes` slice
/// into the original buffer, avoiding memcpy for large payloads.
pub fn decode_push_batch_zero_copy(data: Bytes) -> Result<(String, Vec<Bytes>), ProtocolError> {
    let mut pos = 0;

    // Command byte
    if data.is_empty() {
        return Err(ProtocolError::InsufficientData {
            need: 1,
            have: 0,
        });
    }
    let cmd = BinaryCommand::try_from(data[pos])?;
    if cmd != BinaryCommand::PushBatch {
        return Err(ProtocolError::InvalidCommand(data[pos]));
    }
    pos += 1;

    // Queue name length
    if data.len() < pos + 2 {
        return Err(ProtocolError::InsufficientData {
            need: pos + 2,
            have: data.len(),
        });
    }
    let queue_len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
    pos += 2;

    // Queue name
    if data.len() < pos + queue_len {
        return Err(ProtocolError::InsufficientData {
            need: pos + queue_len,
            have: data.len(),
        });
    }
    let queue = std::str::from_utf8(&data[pos..pos + queue_len])
        .map_err(|_| ProtocolError::InvalidUtf8)?
        .to_string();
    pos += queue_len;

    // Batch count
    if data.len() < pos + 4 {
        return Err(ProtocolError::InsufficientData {
            need: pos + 4,
            have: data.len(),
        });
    }
    let count = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
    pos += 4;

    // Payloads -- zero-copy slicing via Bytes::slice
    let mut payloads = Vec::with_capacity(count);
    for _ in 0..count {
        if data.len() < pos + 4 {
            return Err(ProtocolError::InsufficientData {
                need: pos + 4,
                have: data.len(),
            });
        }
        let msg_len =
            u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 4;

        if msg_len > MAX_PAYLOAD_SIZE {
            return Err(ProtocolError::PayloadTooLarge { size: msg_len });
        }

        if data.len() < pos + msg_len {
            return Err(ProtocolError::InsufficientData {
                need: pos + msg_len,
                have: data.len(),
            });
        }
        payloads.push(data.slice(pos..pos + msg_len));
        pos += msg_len;
    }

    Ok((queue, payloads))
}

/// Encode a push-batch response containing assigned job UUIDs.
///
/// Layout:
/// - 1 byte status (0x00 = ok)
/// - 4 bytes count (BE u32)
/// - For each job: 16 bytes UUID
pub fn encode_push_response(job_ids: &[uuid::Uuid]) -> Vec<u8> {
    let total = 1 + 4 + job_ids.len() * 16;
    let mut buf = Vec::with_capacity(total);
    buf.push(0x00); // status = ok
    buf.extend_from_slice(&(job_ids.len() as u32).to_be_bytes());
    for id in job_ids {
        buf.extend_from_slice(id.as_bytes());
    }
    buf
}

/// Encode a binary error response.
///
/// Layout:
/// - 1 byte status (0x01 = error)
/// - 2 bytes message length (BE u16)
/// - N bytes message (UTF-8)
pub fn encode_error_response(message: &str) -> Vec<u8> {
    let msg_bytes = message.as_bytes();
    let total = 1 + 2 + msg_bytes.len();
    let mut buf = Vec::with_capacity(total);
    buf.push(0x01); // status = error
    buf.extend_from_slice(&(msg_bytes.len() as u16).to_be_bytes());
    buf.extend_from_slice(msg_bytes);
    buf
}

/// Encode a pull-batch request into the binary wire format.
///
/// Layout:
/// - 1 byte command (0x02)
/// - 2 bytes queue name length (BE u16)
/// - N bytes queue name (UTF-8)
/// - 4 bytes count (BE u32)
pub fn encode_pull_batch(queue: &str, count: u32) -> Vec<u8> {
    let queue_bytes = queue.as_bytes();
    let total = 1 + 2 + queue_bytes.len() + 4;
    let mut buf = Vec::with_capacity(total);
    buf.push(BinaryCommand::PullBatch as u8);
    buf.extend_from_slice(&(queue_bytes.len() as u16).to_be_bytes());
    buf.extend_from_slice(queue_bytes);
    buf.extend_from_slice(&count.to_be_bytes());
    buf
}

/// Decode a pull-batch request from the binary wire format.
///
/// Returns `(queue_name, count)` on success.
pub fn decode_pull_batch(data: &[u8]) -> Result<(String, u32), ProtocolError> {
    let mut pos = 0;

    // Command byte
    if data.is_empty() {
        return Err(ProtocolError::InsufficientData {
            need: 1,
            have: 0,
        });
    }
    let cmd = BinaryCommand::try_from(data[pos])?;
    if cmd != BinaryCommand::PullBatch {
        return Err(ProtocolError::InvalidCommand(data[pos]));
    }
    pos += 1;

    // Queue name length
    if data.len() < pos + 2 {
        return Err(ProtocolError::InsufficientData {
            need: pos + 2,
            have: data.len(),
        });
    }
    let queue_len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
    pos += 2;

    // Queue name
    if data.len() < pos + queue_len {
        return Err(ProtocolError::InsufficientData {
            need: pos + queue_len,
            have: data.len(),
        });
    }
    let queue = std::str::from_utf8(&data[pos..pos + queue_len])
        .map_err(|_| ProtocolError::InvalidUtf8)?
        .to_string();
    pos += queue_len;

    // Count
    if data.len() < pos + 4 {
        return Err(ProtocolError::InsufficientData {
            need: pos + 4,
            have: data.len(),
        });
    }
    let count = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);

    Ok((queue, count))
}

/// Encode a pull-batch response containing serialized job payloads.
///
/// Layout:
/// - 1 byte status (0x00 = ok)
/// - 4 bytes count (BE u32)
/// - For each job: 16 bytes UUID + 4 bytes payload length + N bytes JSON payload
pub fn encode_pull_response(jobs: &[(uuid::Uuid, &[u8])]) -> Vec<u8> {
    let payload_size: usize = jobs.iter().map(|(_, p)| 16 + 4 + p.len()).sum();
    let total = 1 + 4 + payload_size;
    let mut buf = Vec::with_capacity(total);
    buf.push(0x00); // status = ok
    buf.extend_from_slice(&(jobs.len() as u32).to_be_bytes());
    for (id, payload) in jobs {
        buf.extend_from_slice(id.as_bytes());
        buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        buf.extend_from_slice(payload);
    }
    buf
}

/// Encode an ack-batch request into the binary wire format.
///
/// Layout:
/// - 1 byte command (0x03)
/// - 4 bytes count (BE u32)
/// - For each job: 16 bytes UUID
pub fn encode_ack_batch(job_ids: &[uuid::Uuid]) -> Vec<u8> {
    let total = 1 + 4 + job_ids.len() * 16;
    let mut buf = Vec::with_capacity(total);
    buf.push(BinaryCommand::AckBatch as u8);
    buf.extend_from_slice(&(job_ids.len() as u32).to_be_bytes());
    for id in job_ids {
        buf.extend_from_slice(id.as_bytes());
    }
    buf
}

/// Decode an ack-batch request from the binary wire format.
///
/// Returns a list of job UUIDs to acknowledge.
pub fn decode_ack_batch(data: &[u8]) -> Result<Vec<uuid::Uuid>, ProtocolError> {
    let mut pos = 0;

    // Command byte
    if data.is_empty() {
        return Err(ProtocolError::InsufficientData {
            need: 1,
            have: 0,
        });
    }
    let cmd = BinaryCommand::try_from(data[pos])?;
    if cmd != BinaryCommand::AckBatch {
        return Err(ProtocolError::InvalidCommand(data[pos]));
    }
    pos += 1;

    // Count
    if data.len() < pos + 4 {
        return Err(ProtocolError::InsufficientData {
            need: pos + 4,
            have: data.len(),
        });
    }
    let count =
        u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
    pos += 4;

    // UUIDs
    let mut ids = Vec::with_capacity(count);
    for _ in 0..count {
        if data.len() < pos + 16 {
            return Err(ProtocolError::InsufficientData {
                need: pos + 16,
                have: data.len(),
            });
        }
        let bytes: [u8; 16] = data[pos..pos + 16]
            .try_into()
            .expect("slice length is 16");
        ids.push(uuid::Uuid::from_bytes(bytes));
        pos += 16;
    }

    Ok(ids)
}

/// Encode an ack-batch response.
///
/// Layout:
/// - 1 byte status (0x00 = ok)
/// - 4 bytes acked count (BE u32)
/// - 4 bytes failed count (BE u32)
pub fn encode_ack_response(acked: u32, failed: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(9);
    buf.push(0x00); // status = ok
    buf.extend_from_slice(&acked.to_be_bytes());
    buf.extend_from_slice(&failed.to_be_bytes());
    buf
}

/// Wrap an inner binary command in a channel frame.
///
/// Layout:
/// - 1 byte command (0x10 = ChannelFrame)
/// - 2 bytes channel_id (BE u16)
/// - N bytes inner frame (command byte + payload)
pub fn encode_channel_frame(channel_id: u16, inner_frame: &[u8]) -> Vec<u8> {
    let total = 1 + 2 + inner_frame.len();
    let mut buf = Vec::with_capacity(total);
    buf.push(BinaryCommand::ChannelFrame as u8);
    buf.extend_from_slice(&channel_id.to_be_bytes());
    buf.extend_from_slice(inner_frame);
    buf
}

/// Wrap a binary response in a channel frame for multiplexed connections.
///
/// Layout:
/// - 2 bytes channel_id (BE u16)
/// - N bytes inner response
pub fn encode_channel_response(channel_id: u16, inner_response: &[u8]) -> Vec<u8> {
    let total = 2 + inner_response.len();
    let mut buf = Vec::with_capacity(total);
    buf.extend_from_slice(&channel_id.to_be_bytes());
    buf.extend_from_slice(inner_response);
    buf
}

/// Decode a channel frame, extracting the channel_id and inner command data.
///
/// The input `data` must start with the 0x10 command byte.
/// Returns `(channel_id, inner_frame)` where `inner_frame` starts with
/// the inner command byte (0x01-0x03).
pub fn decode_channel_frame(data: &[u8]) -> Result<(u16, &[u8]), ProtocolError> {
    if data.is_empty() {
        return Err(ProtocolError::InsufficientData { need: 1, have: 0 });
    }
    let cmd = BinaryCommand::try_from(data[0])?;
    if cmd != BinaryCommand::ChannelFrame {
        return Err(ProtocolError::InvalidCommand(data[0]));
    }
    if data.len() < 4 {
        // Need: 1 (cmd) + 2 (channel_id) + 1 (inner cmd) minimum
        return Err(ProtocolError::InsufficientData {
            need: 4,
            have: data.len(),
        });
    }
    let channel_id = u16::from_be_bytes([data[1], data[2]]);
    Ok((channel_id, &data[3..]))
}

/// Decode a push-batch response into a list of job UUIDs.
pub fn decode_push_response(data: &[u8]) -> Result<Vec<uuid::Uuid>, ProtocolError> {
    let mut pos = 0;

    // Status byte
    if data.is_empty() {
        return Err(ProtocolError::InsufficientData {
            need: 1,
            have: 0,
        });
    }
    let _status = data[pos];
    pos += 1;

    // Count
    if data.len() < pos + 4 {
        return Err(ProtocolError::InsufficientData {
            need: pos + 4,
            have: data.len(),
        });
    }
    let count =
        u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
    pos += 4;

    // UUIDs
    let mut ids = Vec::with_capacity(count);
    for _ in 0..count {
        if data.len() < pos + 16 {
            return Err(ProtocolError::InsufficientData {
                need: pos + 16,
                have: data.len(),
            });
        }
        let bytes: [u8; 16] = data[pos..pos + 16]
            .try_into()
            .expect("slice length is 16");
        ids.push(uuid::Uuid::from_bytes(bytes));
        pos += 16;
    }

    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_push_batch_roundtrip() {
        let payloads: Vec<&[u8]> = vec![b"hello", b"world", b"test"];
        let encoded = encode_push_batch("my-queue", &payloads);
        let (queue, decoded) = decode_push_batch(&encoded).unwrap();
        assert_eq!(queue, "my-queue");
        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded[0], b"hello");
        assert_eq!(decoded[1], b"world");
        assert_eq!(decoded[2], b"test");
    }

    #[test]
    fn encode_decode_response_roundtrip() {
        let ids: Vec<uuid::Uuid> = (0..5)
            .map(|i: u8| {
                let mut bytes = [0u8; 16];
                bytes[0] = i;
                bytes[15] = 0xFF - i;
                uuid::Uuid::from_bytes(bytes)
            })
            .collect();
        let encoded = encode_push_response(&ids);
        let decoded = decode_push_response(&encoded).unwrap();
        assert_eq!(ids, decoded);
    }

    #[test]
    fn decode_empty_batch() {
        let encoded = encode_push_batch("q", &[]);
        let (queue, payloads) = decode_push_batch(&encoded).unwrap();
        assert_eq!(queue, "q");
        assert!(payloads.is_empty());
    }

    #[test]
    fn decode_truncated_data_returns_error() {
        let encoded = encode_push_batch("q", &[b"data"]);
        let truncated = &encoded[..encoded.len() / 2];
        assert!(decode_push_batch(truncated).is_err());
    }

    #[test]
    fn large_batch_1000_messages() {
        let msgs: Vec<Vec<u8>> = (0..1000).map(|i| format!("msg-{i}").into_bytes()).collect();
        let refs: Vec<&[u8]> = msgs.iter().map(|m| m.as_slice()).collect();
        let encoded = encode_push_batch("bulk", &refs);
        let (queue, decoded) = decode_push_batch(&encoded).unwrap();
        assert_eq!(queue, "bulk");
        assert_eq!(decoded.len(), 1000);
        assert_eq!(decoded[999], format!("msg-999").into_bytes());
    }

    #[test]
    fn encode_decode_pull_batch_roundtrip() {
        let encoded = encode_pull_batch("work-queue", 10);
        let (queue, count) = decode_pull_batch(&encoded).unwrap();
        assert_eq!(queue, "work-queue");
        assert_eq!(count, 10);
    }

    #[test]
    fn encode_decode_ack_batch_roundtrip() {
        let ids: Vec<uuid::Uuid> = (0..3)
            .map(|i: u8| {
                let mut bytes = [0u8; 16];
                bytes[0] = i;
                uuid::Uuid::from_bytes(bytes)
            })
            .collect();
        let encoded = encode_ack_batch(&ids);
        let decoded = decode_ack_batch(&encoded).unwrap();
        assert_eq!(ids, decoded);
    }

    #[test]
    fn encode_decode_ack_batch_empty() {
        let encoded = encode_ack_batch(&[]);
        let decoded = decode_ack_batch(&encoded).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn error_response_roundtrip() {
        let encoded = encode_error_response("something went wrong");
        assert_eq!(encoded[0], 0x01); // error status
        let msg_len = u16::from_be_bytes([encoded[1], encoded[2]]) as usize;
        let msg = std::str::from_utf8(&encoded[3..3 + msg_len]).unwrap();
        assert_eq!(msg, "something went wrong");
    }

    #[test]
    fn zero_copy_decode_push_batch_roundtrip() {
        let payloads: Vec<&[u8]> = vec![b"hello", b"world", b"test"];
        let encoded = encode_push_batch("my-queue", &payloads);
        let data = Bytes::from(encoded);
        let (queue, decoded) = decode_push_batch_zero_copy(data).unwrap();
        assert_eq!(queue, "my-queue");
        assert_eq!(decoded.len(), 3);
        assert_eq!(&decoded[0][..], b"hello");
        assert_eq!(&decoded[1][..], b"world");
        assert_eq!(&decoded[2][..], b"test");
    }

    #[test]
    fn zero_copy_decode_empty_batch() {
        let encoded = encode_push_batch("q", &[]);
        let data = Bytes::from(encoded);
        let (queue, payloads) = decode_push_batch_zero_copy(data).unwrap();
        assert_eq!(queue, "q");
        assert!(payloads.is_empty());
    }

    #[test]
    fn ack_response_encoding() {
        let encoded = encode_ack_response(5, 2);
        assert_eq!(encoded[0], 0x00); // ok status
        let acked = u32::from_be_bytes([encoded[1], encoded[2], encoded[3], encoded[4]]);
        let failed = u32::from_be_bytes([encoded[5], encoded[6], encoded[7], encoded[8]]);
        assert_eq!(acked, 5);
        assert_eq!(failed, 2);
    }

    #[test]
    fn encode_decode_channel_frame_roundtrip() {
        let inner = encode_push_batch("q", &[b"data"]);
        let encoded = encode_channel_frame(42, &inner);

        // First byte should be 0x10
        assert_eq!(encoded[0], BinaryCommand::ChannelFrame as u8);

        let (channel_id, inner_data) = decode_channel_frame(&encoded).unwrap();
        assert_eq!(channel_id, 42);
        assert_eq!(inner_data, inner.as_slice());
    }

    #[test]
    fn channel_frame_zero_id() {
        let inner = encode_pull_batch("q", 5);
        let encoded = encode_channel_frame(0, &inner);
        let (channel_id, inner_data) = decode_channel_frame(&encoded).unwrap();
        assert_eq!(channel_id, 0);
        assert_eq!(inner_data, inner.as_slice());
    }

    #[test]
    fn channel_response_encoding() {
        let inner_resp = encode_push_response(&[]);
        let encoded = encode_channel_response(99, &inner_resp);
        // First 2 bytes are channel_id
        let channel_id = u16::from_be_bytes([encoded[0], encoded[1]]);
        assert_eq!(channel_id, 99);
        // Rest is the inner response
        assert_eq!(&encoded[2..], inner_resp.as_slice());
    }

    #[test]
    fn channel_frame_truncated_returns_error() {
        // Too short: just 0x10 + 1 byte
        assert!(decode_channel_frame(&[0x10, 0x00]).is_err());
    }
}
