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

/// Start accepting TLS-encrypted TCP connections on the given listener.
///
/// Performs a TLS handshake on each incoming connection using the provided
/// [`tokio_rustls::TlsAcceptor`], then delegates to [`handler::handle_connection`].
/// Connections that fail the TLS handshake are logged and dropped.
///
/// This function is only available when the `tls` feature is enabled.
#[cfg(feature = "tls")]
pub async fn start_tls_tcp_server(
    listener: TcpListener,
    manager: Arc<QueueManager>,
    auth_config: AuthConfig,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
    acceptor: tokio_rustls::TlsAcceptor,
) {
    let auth = Arc::new(auth_config);
    let addr = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    info!(addr = %addr, "TLS TCP server listening");

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _)) => {
                        let mgr = Arc::clone(&manager);
                        let auth = Arc::clone(&auth);
                        let acceptor = acceptor.clone();
                        tokio::spawn(async move {
                            match acceptor.accept(stream).await {
                                Ok(tls_stream) => {
                                    handler::handle_connection(tls_stream, mgr, &auth).await;
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "TLS handshake failed");
                                }
                            }
                        });
                    }
                    Err(e) => {
                        error!(error = %e, "Failed to accept TCP connection");
                    }
                }
            }
            _ = shutdown_rx.changed() => {
                info!("TLS TCP server shutting down");
                break;
            }
        }
    }
}

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
