//! Integration tests for TCP connection-level bearer token authentication.

use std::sync::Arc;

use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use rustqueue::config::AuthConfig;
use rustqueue::engine::queue::QueueManager;
use rustqueue::protocol;
use rustqueue::storage::RedbStorage;

// ── Test helpers ─────────────────────────────────────────────────────────────

/// Start a TCP server with a given auth config and return the port number.
/// The tempdir is leaked intentionally so it outlives the spawned server task.
async fn start_test_tcp_server_with_auth(auth_config: AuthConfig) -> u16 {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.redb");
    // Leak the tempdir so it lives for the duration of the process.
    let _keep = Box::leak(Box::new(dir));

    let storage = Arc::new(RedbStorage::new(&db_path).unwrap());
    let qm = Arc::new(QueueManager::new(storage));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

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

/// When auth is disabled, commands should work without any auth handshake.
#[tokio::test]
async fn test_tcp_auth_disabled_allows_commands() {
    let auth_config = AuthConfig {
        enabled: false,
        tokens: vec![],
    };
    let port = start_test_tcp_server_with_auth(auth_config).await;
    let (mut reader, mut writer) = connect_tcp(port).await;

    // Push should succeed immediately without auth
    send_cmd(
        &mut writer,
        json!({"cmd": "push", "queue": "test-q", "name": "test-job", "data": {"key": "value"}}),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(
        resp["ok"].as_bool().unwrap(),
        "push should succeed when auth is disabled"
    );
    assert!(
        resp["id"].as_str().is_some(),
        "push response should contain a job id"
    );

    // Stats should also work
    send_cmd(
        &mut writer,
        json!({"cmd": "stats", "queue": "test-q"}),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(
        resp["ok"].as_bool().unwrap(),
        "stats should succeed when auth is disabled"
    );
}

/// When auth is enabled, sending a non-auth command before authenticating
/// should return an UNAUTHORIZED error.
#[tokio::test]
async fn test_tcp_auth_required_rejects_unauthenticated() {
    let auth_config = AuthConfig {
        enabled: true,
        tokens: vec!["valid-token-123".to_string()],
    };
    let port = start_test_tcp_server_with_auth(auth_config).await;
    let (mut reader, mut writer) = connect_tcp(port).await;

    // Attempt a push without authenticating first
    send_cmd(
        &mut writer,
        json!({"cmd": "push", "queue": "test-q", "name": "test-job", "data": {}}),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(
        !resp["ok"].as_bool().unwrap(),
        "push should be rejected when not authenticated"
    );
    assert_eq!(
        resp["error"]["code"].as_str().unwrap(),
        "UNAUTHORIZED",
        "error code should be UNAUTHORIZED"
    );
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Authentication required"),
        "error message should mention authentication"
    );

    // Also verify stats is rejected
    send_cmd(
        &mut writer,
        json!({"cmd": "stats", "queue": "test-q"}),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(
        !resp["ok"].as_bool().unwrap(),
        "stats should also be rejected when not authenticated"
    );
    assert_eq!(
        resp["error"]["code"].as_str().unwrap(),
        "UNAUTHORIZED",
    );
}

/// Full auth flow: authenticate first, then use regular commands.
#[tokio::test]
async fn test_tcp_auth_flow() {
    let token = "my-secret-token-abc";
    let auth_config = AuthConfig {
        enabled: true,
        tokens: vec![token.to_string()],
    };
    let port = start_test_tcp_server_with_auth(auth_config).await;
    let (mut reader, mut writer) = connect_tcp(port).await;

    // 1. Try auth with an invalid token first
    send_cmd(
        &mut writer,
        json!({"cmd": "auth", "token": "wrong-token"}),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(
        !resp["ok"].as_bool().unwrap(),
        "auth with wrong token should fail"
    );
    assert_eq!(
        resp["error"]["code"].as_str().unwrap(),
        "UNAUTHORIZED",
        "wrong token should return UNAUTHORIZED"
    );

    // 2. Commands should still be rejected after failed auth
    send_cmd(
        &mut writer,
        json!({"cmd": "push", "queue": "q", "name": "j", "data": {}}),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(
        !resp["ok"].as_bool().unwrap(),
        "push should still be rejected after failed auth attempt"
    );

    // 3. Authenticate with the valid token
    send_cmd(
        &mut writer,
        json!({"cmd": "auth", "token": token}),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(
        resp["ok"].as_bool().unwrap(),
        "auth with valid token should succeed"
    );

    // 4. Push should now work
    send_cmd(
        &mut writer,
        json!({"cmd": "push", "queue": "auth-q", "name": "auth-job", "data": {"x": 1}}),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(
        resp["ok"].as_bool().unwrap(),
        "push should succeed after authentication"
    );
    let job_id = resp["id"].as_str().unwrap().to_string();

    // 5. Pull should work too
    send_cmd(
        &mut writer,
        json!({"cmd": "pull", "queue": "auth-q"}),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(
        resp["ok"].as_bool().unwrap(),
        "pull should succeed after authentication"
    );
    assert_eq!(
        resp["job"]["id"].as_str().unwrap(),
        job_id,
        "pulled job id should match pushed job id"
    );

    // 6. Ack should work
    send_cmd(
        &mut writer,
        json!({"cmd": "ack", "id": job_id}),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(
        resp["ok"].as_bool().unwrap(),
        "ack should succeed after authentication"
    );
}
