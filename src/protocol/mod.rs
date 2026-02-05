//! TCP protocol module — newline-delimited JSON interface to RustQueue.
//!
//! Provides [`start_tcp_server`] which binds to a [`TcpListener`](tokio::net::TcpListener)
//! and spawns a handler task for each incoming connection.

pub mod handler;

use std::sync::Arc;

use tokio::net::TcpListener;
use tracing::{error, info};

use crate::engine::queue::QueueManager;

/// Start accepting TCP connections on the given listener.
///
/// Each connection is handled in its own tokio task via [`handler::handle_connection`].
/// This function runs indefinitely until the listener is dropped or an accept error occurs.
pub async fn start_tcp_server(listener: TcpListener, manager: Arc<QueueManager>) {
    let addr = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    info!(addr = %addr, "TCP server listening");

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let mgr = Arc::clone(&manager);
                tokio::spawn(async move {
                    handler::handle_connection(stream, mgr).await;
                });
            }
            Err(e) => {
                error!(error = %e, "Failed to accept TCP connection");
            }
        }
    }
}
