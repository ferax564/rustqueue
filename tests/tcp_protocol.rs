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

    tokio::spawn(async move {
        protocol::start_tcp_server(listener, qm, auth_config).await;
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
