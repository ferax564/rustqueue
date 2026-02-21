//! Integration tests for the TCP protocol interface.

use std::sync::Arc;

use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use rustqueue::config::AuthConfig;
use rustqueue::engine::queue::QueueManager;
use rustqueue::protocol;
use rustqueue::protocol::binary;
use rustqueue::storage::RedbStorage;

// ── Test helpers ─────────────────────────────────────────────────────────────

/// Start a TCP server on a random port and return the port number.
/// Auth is disabled so existing tests continue to work without changes.
/// The tempdir is leaked intentionally so it outlives the spawned server task.
async fn start_test_tcp_server() -> u16 {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.redb");
    // Leak the tempdir so it lives for the duration of the process.
    let _keep = Box::leak(Box::new(dir));

    let storage = Arc::new(RedbStorage::new(&db_path).unwrap());
    let qm = Arc::new(QueueManager::new(storage));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let auth_config = AuthConfig {
        enabled: false,
        tokens: vec![],
    };

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    // Leak the sender so it outlives the spawned server task (never sends shutdown).
    let _keep_tx = Box::leak(Box::new(shutdown_tx));
    tokio::spawn(async move {
        protocol::start_tcp_server(listener, qm, auth_config, shutdown_rx).await;
    });

    port
}

async fn connect_tcp(
    port: u16,
) -> (
    BufReader<tokio::net::tcp::OwnedReadHalf>,
    tokio::net::tcp::OwnedWriteHalf,
) {
    let stream = TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    let (reader, writer) = stream.into_split();
    (BufReader::new(reader), writer)
}

async fn send_cmd(writer: &mut tokio::net::tcp::OwnedWriteHalf, cmd: serde_json::Value) {
    let mut line = serde_json::to_string(&cmd).unwrap();
    line.push('\n');
    writer.write_all(line.as_bytes()).await.unwrap();
}

async fn read_response(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
) -> serde_json::Value {
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    serde_json::from_str(&line).unwrap()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_tcp_push_and_pull() {
    let port = start_test_tcp_server().await;
    let (mut reader, mut writer) = connect_tcp(port).await;

    // Push
    send_cmd(
        &mut writer,
        json!({"cmd": "push", "queue": "q", "name": "j", "data": {}}),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(resp["ok"].as_bool().unwrap());
    let job_id = resp["id"].as_str().unwrap().to_string();

    // Pull
    send_cmd(&mut writer, json!({"cmd": "pull", "queue": "q"})).await;
    let resp = read_response(&mut reader).await;
    assert!(resp["ok"].as_bool().unwrap());
    assert_eq!(resp["job"]["id"].as_str().unwrap(), job_id);

    // Ack
    send_cmd(&mut writer, json!({"cmd": "ack", "id": job_id})).await;
    let resp = read_response(&mut reader).await;
    assert!(resp["ok"].as_bool().unwrap());
}

#[tokio::test]
async fn test_tcp_push_batch_and_ack_batch() {
    let port = start_test_tcp_server().await;
    let (mut reader, mut writer) = connect_tcp(port).await;

    send_cmd(
        &mut writer,
        json!({
            "cmd": "push_batch",
            "queue": "q-batch",
            "jobs": [
                {"name": "j1", "data": {"i": 1}},
                {"name": "j2", "data": {"i": 2}},
                {"name": "j3", "data": {"i": 3}}
            ]
        }),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(resp["ok"].as_bool().unwrap(), "push_batch failed: {resp}");
    let ids = resp["ids"].as_array().unwrap();
    assert_eq!(ids.len(), 3);

    send_cmd(
        &mut writer,
        json!({"cmd": "pull", "queue": "q-batch", "count": 3}),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(resp["ok"].as_bool().unwrap(), "pull failed: {resp}");
    let jobs = resp["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 3);

    let ack_items: Vec<serde_json::Value> = jobs
        .iter()
        .map(|job| json!({"id": job["id"].as_str().unwrap()}))
        .collect();
    send_cmd(&mut writer, json!({"cmd": "ack_batch", "items": ack_items})).await;
    let resp = read_response(&mut reader).await;
    assert!(resp["ok"].as_bool().unwrap(), "ack_batch failed: {resp}");
    assert_eq!(resp["acked"].as_u64().unwrap(), 3);
    assert_eq!(resp["failed"].as_u64().unwrap(), 0);
    let results = resp["results"].as_array().unwrap();
    assert_eq!(results.len(), 3);
    assert!(results.iter().all(|r| r["ok"].as_bool() == Some(true)));
}

#[tokio::test]
async fn test_tcp_ack_batch_partial_failure() {
    let port = start_test_tcp_server().await;
    let (mut reader, mut writer) = connect_tcp(port).await;

    send_cmd(
        &mut writer,
        json!({"cmd": "push", "queue": "q-ack", "name": "j", "data": {}}),
    )
    .await;
    let pushed = read_response(&mut reader).await;
    assert!(pushed["ok"].as_bool().unwrap());
    let job_id = pushed["id"].as_str().unwrap().to_string();

    send_cmd(&mut writer, json!({"cmd": "pull", "queue": "q-ack"})).await;
    let pulled = read_response(&mut reader).await;
    assert!(pulled["ok"].as_bool().unwrap());

    let missing_id = uuid::Uuid::now_v7().to_string();
    send_cmd(
        &mut writer,
        json!({
            "cmd": "ack_batch",
            "ids": [job_id, missing_id]
        }),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(
        !resp["ok"].as_bool().unwrap(),
        "ack_batch should be partial failure"
    );
    assert_eq!(resp["acked"].as_u64().unwrap(), 1);
    assert_eq!(resp["failed"].as_u64().unwrap(), 1);
}

#[tokio::test]
async fn test_tcp_invalid_command() {
    let port = start_test_tcp_server().await;
    let (mut reader, mut writer) = connect_tcp(port).await;

    send_cmd(&mut writer, json!({"cmd": "invalid"})).await;
    let resp = read_response(&mut reader).await;
    assert!(!resp["ok"].as_bool().unwrap());
    assert!(resp["error"]["code"].as_str().is_some());
}

#[tokio::test]
async fn test_tcp_malformed_json() {
    let port = start_test_tcp_server().await;
    let stream = TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    writer.write_all(b"not json\n").await.unwrap();
    let resp = read_response(&mut reader).await;
    assert!(!resp["ok"].as_bool().unwrap());
}

#[tokio::test]
async fn test_tcp_schedule_create_and_list() {
    let port = start_test_tcp_server().await;
    let (mut reader, mut writer) = connect_tcp(port).await;

    // Create a schedule
    send_cmd(
        &mut writer,
        json!({
            "cmd": "schedule_create",
            "name": "daily-report",
            "queue": "reports",
            "job_name": "generate-report",
            "job_data": {"type": "daily"},
            "every_ms": 86400000_u64,
        }),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(
        resp["ok"].as_bool().unwrap(),
        "schedule_create failed: {resp}"
    );

    // List schedules
    send_cmd(&mut writer, json!({"cmd": "schedule_list"})).await;
    let resp = read_response(&mut reader).await;
    assert!(
        resp["ok"].as_bool().unwrap(),
        "schedule_list failed: {resp}"
    );

    let schedules = resp["schedules"].as_array().unwrap();
    assert_eq!(schedules.len(), 1);
    assert_eq!(schedules[0]["name"].as_str().unwrap(), "daily-report");
    assert_eq!(schedules[0]["queue"].as_str().unwrap(), "reports");
    assert_eq!(
        schedules[0]["job_name"].as_str().unwrap(),
        "generate-report"
    );
    assert_eq!(schedules[0]["every_ms"].as_u64().unwrap(), 86400000);
}

#[tokio::test]
async fn test_tcp_schedule_pause_resume() {
    let port = start_test_tcp_server().await;
    let (mut reader, mut writer) = connect_tcp(port).await;

    // Create a schedule
    send_cmd(
        &mut writer,
        json!({
            "cmd": "schedule_create",
            "name": "hourly-sync",
            "queue": "sync",
            "job_name": "sync-data",
            "every_ms": 3600000_u64,
        }),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(
        resp["ok"].as_bool().unwrap(),
        "schedule_create failed: {resp}"
    );

    // Pause the schedule
    send_cmd(
        &mut writer,
        json!({"cmd": "schedule_pause", "name": "hourly-sync"}),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(
        resp["ok"].as_bool().unwrap(),
        "schedule_pause failed: {resp}"
    );

    // Get and verify paused
    send_cmd(
        &mut writer,
        json!({"cmd": "schedule_get", "name": "hourly-sync"}),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(resp["ok"].as_bool().unwrap(), "schedule_get failed: {resp}");
    assert!(
        resp["schedule"]["paused"].as_bool().unwrap(),
        "schedule should be paused"
    );

    // Resume the schedule
    send_cmd(
        &mut writer,
        json!({"cmd": "schedule_resume", "name": "hourly-sync"}),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(
        resp["ok"].as_bool().unwrap(),
        "schedule_resume failed: {resp}"
    );

    // Get and verify resumed
    send_cmd(
        &mut writer,
        json!({"cmd": "schedule_get", "name": "hourly-sync"}),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(resp["ok"].as_bool().unwrap(), "schedule_get failed: {resp}");
    assert!(
        !resp["schedule"]["paused"].as_bool().unwrap(),
        "schedule should not be paused"
    );
}

// ── Binary protocol integration tests ──────────────────────────────────────

#[tokio::test]
async fn test_binary_push_batch_over_tcp() {
    let port = start_test_tcp_server().await;
    let stream = TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    let (mut read_half, mut write_half) = stream.into_split();

    // Encode a binary push batch with 3 JSON payloads
    let payloads: Vec<&[u8]> = vec![
        br#"{"task":"a"}"#,
        br#"{"task":"b"}"#,
        br#"{"task":"c"}"#,
    ];
    let frame = binary::encode_push_batch("binary-q", &payloads);
    write_half.write_all(&frame).await.unwrap();

    // Read binary response: 1 byte status + 4 bytes count + N * 16 bytes UUIDs
    let mut status = [0u8; 1];
    read_half.read_exact(&mut status).await.unwrap();
    assert_eq!(status[0], 0x00, "Expected success status");

    let mut count_buf = [0u8; 4];
    read_half.read_exact(&mut count_buf).await.unwrap();
    let count = u32::from_be_bytes(count_buf) as usize;
    assert_eq!(count, 3, "Expected 3 job IDs in response");

    let mut uuid_buf = vec![0u8; count * 16];
    read_half.read_exact(&mut uuid_buf).await.unwrap();

    // Parse the UUIDs and verify they are valid
    for i in 0..count {
        let bytes: [u8; 16] = uuid_buf[i * 16..(i + 1) * 16].try_into().unwrap();
        let id = uuid::Uuid::from_bytes(bytes);
        assert!(!id.is_nil(), "Job ID should not be nil");
    }
}

#[tokio::test]
async fn test_binary_push_then_json_pull() {
    let port = start_test_tcp_server().await;

    // First connection: binary push
    let stream = TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    let (mut read_half, mut write_half) = stream.into_split();

    let payloads: Vec<&[u8]> = vec![br#"{"x":42}"#];
    let frame = binary::encode_push_batch("mixed-q", &payloads);
    write_half.write_all(&frame).await.unwrap();

    // Read and parse push response
    let mut resp_buf = vec![0u8; 1 + 4 + 16];
    read_half.read_exact(&mut resp_buf).await.unwrap();
    assert_eq!(resp_buf[0], 0x00);
    let count = u32::from_be_bytes([resp_buf[1], resp_buf[2], resp_buf[3], resp_buf[4]]) as usize;
    assert_eq!(count, 1);
    let pushed_id_bytes: [u8; 16] = resp_buf[5..21].try_into().unwrap();
    let pushed_id = uuid::Uuid::from_bytes(pushed_id_bytes);
    drop(read_half);
    drop(write_half);

    // Second connection: JSON pull
    let (mut reader, mut writer) = connect_tcp(port).await;
    send_cmd(
        &mut writer,
        json!({"cmd": "pull", "queue": "mixed-q"}),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(resp["ok"].as_bool().unwrap(), "pull failed: {resp}");
    let job = &resp["job"];
    assert_eq!(
        job["id"].as_str().unwrap(),
        pushed_id.to_string(),
        "Pulled job should match binary-pushed job"
    );
    assert_eq!(job["data"]["x"].as_u64().unwrap(), 42);
}

#[tokio::test]
async fn test_binary_full_lifecycle_push_pull_ack() {
    let port = start_test_tcp_server().await;
    let stream = TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    let (mut read_half, mut write_half) = stream.into_split();

    // 1. Binary push batch
    let payloads: Vec<&[u8]> = vec![br#"{"step":"one"}"#, br#"{"step":"two"}"#];
    let frame = binary::encode_push_batch("lifecycle-q", &payloads);
    write_half.write_all(&frame).await.unwrap();

    // Read push response
    let mut resp_buf = vec![0u8; 1 + 4 + 2 * 16];
    read_half.read_exact(&mut resp_buf).await.unwrap();
    assert_eq!(resp_buf[0], 0x00, "push should succeed");
    let resp_ids = binary::decode_push_response(&resp_buf).unwrap();
    assert_eq!(resp_ids.len(), 2);

    // 2. Binary pull batch
    let pull_frame = binary::encode_pull_batch("lifecycle-q", 2);
    write_half.write_all(&pull_frame).await.unwrap();

    // Read pull response: status + count + (uuid + payload_len + payload) * N
    let mut pull_status = [0u8; 1];
    read_half.read_exact(&mut pull_status).await.unwrap();
    assert_eq!(pull_status[0], 0x00, "pull should succeed");

    let mut pull_count_buf = [0u8; 4];
    read_half.read_exact(&mut pull_count_buf).await.unwrap();
    let pull_count = u32::from_be_bytes(pull_count_buf) as usize;
    assert_eq!(pull_count, 2, "should pull 2 jobs");

    let mut pulled_ids = Vec::new();
    for _ in 0..pull_count {
        let mut id_buf = [0u8; 16];
        read_half.read_exact(&mut id_buf).await.unwrap();
        pulled_ids.push(uuid::Uuid::from_bytes(id_buf));

        let mut len_buf = [0u8; 4];
        read_half.read_exact(&mut len_buf).await.unwrap();
        let payload_len = u32::from_be_bytes(len_buf) as usize;

        let mut payload = vec![0u8; payload_len];
        read_half.read_exact(&mut payload).await.unwrap();
    }

    // 3. Binary ack batch
    let ack_frame = binary::encode_ack_batch(&pulled_ids);
    write_half.write_all(&ack_frame).await.unwrap();

    // Read ack response: status + acked (4) + failed (4)
    let mut ack_resp = [0u8; 9];
    read_half.read_exact(&mut ack_resp).await.unwrap();
    assert_eq!(ack_resp[0], 0x00, "ack should succeed");
    let acked = u32::from_be_bytes([ack_resp[1], ack_resp[2], ack_resp[3], ack_resp[4]]);
    let failed = u32::from_be_bytes([ack_resp[5], ack_resp[6], ack_resp[7], ack_resp[8]]);
    assert_eq!(acked, 2, "should ack 2 jobs");
    assert_eq!(failed, 0, "no failures expected");
}

#[tokio::test]
async fn test_binary_mixed_with_json_on_same_connection() {
    let port = start_test_tcp_server().await;
    let stream = TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    let (reader, writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut writer = writer;

    // 1. Send a JSON push first
    let mut json_cmd = serde_json::to_string(&json!({
        "cmd": "push", "queue": "mixed-conn", "name": "json-job", "data": {"from": "json"}
    }))
    .unwrap();
    json_cmd.push('\n');
    writer.write_all(json_cmd.as_bytes()).await.unwrap();

    // Read JSON response
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert!(resp["ok"].as_bool().unwrap(), "JSON push failed: {resp}");

    // 2. Now send a binary push on the same connection
    let payloads: Vec<&[u8]> = vec![br#"{"from":"binary"}"#];
    let frame = binary::encode_push_batch("mixed-conn", &payloads);
    writer.write_all(&frame).await.unwrap();

    // Read binary response (need raw read, not line-based)
    let mut resp_buf = vec![0u8; 1 + 4 + 16];
    reader.read_exact(&mut resp_buf).await.unwrap();
    assert_eq!(resp_buf[0], 0x00, "binary push should succeed");
    let count = u32::from_be_bytes([resp_buf[1], resp_buf[2], resp_buf[3], resp_buf[4]]);
    assert_eq!(count, 1);

    // 3. Send another JSON command to verify JSON still works
    let mut stats_cmd =
        serde_json::to_string(&json!({"cmd": "stats", "queue": "mixed-conn"})).unwrap();
    stats_cmd.push('\n');
    writer.write_all(stats_cmd.as_bytes()).await.unwrap();

    let mut line2 = String::new();
    reader.read_line(&mut line2).await.unwrap();
    let resp2: serde_json::Value = serde_json::from_str(&line2).unwrap();
    assert!(resp2["ok"].as_bool().unwrap(), "JSON stats failed: {resp2}");
}
