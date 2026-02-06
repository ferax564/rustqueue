//! Integration tests for the TCP protocol interface.

use std::sync::Arc;

use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use rustqueue::config::AuthConfig;
use rustqueue::engine::queue::QueueManager;
use rustqueue::protocol;
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
    assert!(resp["ok"].as_bool().unwrap(), "schedule_create failed: {resp}");

    // List schedules
    send_cmd(&mut writer, json!({"cmd": "schedule_list"})).await;
    let resp = read_response(&mut reader).await;
    assert!(resp["ok"].as_bool().unwrap(), "schedule_list failed: {resp}");

    let schedules = resp["schedules"].as_array().unwrap();
    assert_eq!(schedules.len(), 1);
    assert_eq!(schedules[0]["name"].as_str().unwrap(), "daily-report");
    assert_eq!(schedules[0]["queue"].as_str().unwrap(), "reports");
    assert_eq!(schedules[0]["job_name"].as_str().unwrap(), "generate-report");
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
    assert!(resp["ok"].as_bool().unwrap(), "schedule_create failed: {resp}");

    // Pause the schedule
    send_cmd(
        &mut writer,
        json!({"cmd": "schedule_pause", "name": "hourly-sync"}),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(resp["ok"].as_bool().unwrap(), "schedule_pause failed: {resp}");

    // Get and verify paused
    send_cmd(
        &mut writer,
        json!({"cmd": "schedule_get", "name": "hourly-sync"}),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(resp["ok"].as_bool().unwrap(), "schedule_get failed: {resp}");
    assert!(resp["schedule"]["paused"].as_bool().unwrap(), "schedule should be paused");

    // Resume the schedule
    send_cmd(
        &mut writer,
        json!({"cmd": "schedule_resume", "name": "hourly-sync"}),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(resp["ok"].as_bool().unwrap(), "schedule_resume failed: {resp}");

    // Get and verify resumed
    send_cmd(
        &mut writer,
        json!({"cmd": "schedule_get", "name": "hourly-sync"}),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(resp["ok"].as_bool().unwrap(), "schedule_get failed: {resp}");
    assert!(!resp["schedule"]["paused"].as_bool().unwrap(), "schedule should not be paused");
}
