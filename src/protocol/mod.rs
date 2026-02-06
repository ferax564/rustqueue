//! TCP protocol module — newline-delimited JSON interface to RustQueue.
//!
//! Provides [`start_tcp_server`] which binds to a [`TcpListener`](tokio::net::TcpListener)
//! and spawns a handler task for each incoming connection.

pub mod handler;

use std::sync::Arc;

use tokio::net::TcpListener;
use tracing::{error, info};

use crate::config::AuthConfig;
use crate::engine::queue::QueueManager;

/// Start accepting TCP connections on the given listener.
///
/// Each connection is handled in its own tokio task via [`handler::handle_connection`].
/// This function runs until the `shutdown_rx` watch channel signals shutdown, at which
/// point it stops accepting new connections. In-flight connections continue until they
/// finish their current command.
///
/// The `auth_config` is shared with every connection handler. When auth is enabled,
/// clients must send an `auth` command before any other command is accepted.
pub async fn start_tcp_server(
    listener: TcpListener,
    manager: Arc<QueueManager>,
    auth_config: AuthConfig,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    let addr = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    info!(addr = %addr, "TCP server listening");

    let auth_config = Arc::new(auth_config);

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _addr)) => {
                        let mgr = Arc::clone(&manager);
                        let auth = Arc::clone(&auth_config);
                        tokio::spawn(async move {
                            handler::handle_connection(stream, mgr, &auth).await;
                        });
                    }
                    Err(e) => {
                        error!(error = %e, "Failed to accept TCP connection");
                    }
                }
            }
            _ = shutdown_rx.changed() => {
                info!("TCP server shutting down, no longer accepting connections");
                break;
            }
        }
    }
}
