use std::sync::Arc;

use clap::Parser;
use tracing::{info, warn};


#[cfg(feature = "tls")]
fn load_certs(path: &str) -> anyhow::Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    let certs = rustls_pemfile::certs(&mut reader).collect::<Result<Vec<_>, _>>()?;
    Ok(certs)
}

#[cfg(feature = "tls")]
fn load_key(path: &str) -> anyhow::Result<rustls::pki_types::PrivateKeyDer<'static>> {
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    let key = rustls_pemfile::private_key(&mut reader)?
        .ok_or_else(|| anyhow::anyhow!("No private key found in {}", path))?;
    Ok(key)
}

#[derive(Parser)]
#[command(name = "rustqueue", version, about = "A high-performance distributed job scheduler")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// Start the RustQueue server
    Serve {
        /// Path to config file
        #[arg(short, long, default_value = "rustqueue.toml")]
        config: String,

        /// HTTP port (overrides config file)
        #[arg(long, env = "RUSTQUEUE_HTTP_PORT")]
        http_port: Option<u16>,

        /// TCP port (overrides config file)
        #[arg(long, env = "RUSTQUEUE_TCP_PORT")]
        tcp_port: Option<u16>,
    },

    /// Show queue status (connects to running server)
    #[cfg(feature = "cli")]
    Status {
        /// Server host
        #[arg(long, default_value = "127.0.0.1", env = "RUSTQUEUE_HOST")]
        host: String,
        /// Server HTTP port
        #[arg(long, default_value_t = 6790, env = "RUSTQUEUE_HTTP_PORT")]
        http_port: u16,
    },

    /// Push a job to a queue (connects to running server)
    #[cfg(feature = "cli")]
    Push {
        /// Queue name
        #[arg(long)]
        queue: String,
        /// Job name
        #[arg(long)]
        name: String,
        /// Job data as JSON string
        #[arg(long, default_value = "{}")]
        data: String,
        /// Server host
        #[arg(long, default_value = "127.0.0.1", env = "RUSTQUEUE_HOST")]
        host: String,
        /// Server HTTP port
        #[arg(long, default_value_t = 6790, env = "RUSTQUEUE_HTTP_PORT")]
        http_port: u16,
    },

    /// Inspect a job by ID (connects to running server)
    #[cfg(feature = "cli")]
    Inspect {
        /// Job ID (UUID)
        id: String,
        /// Server host
        #[arg(long, default_value = "127.0.0.1", env = "RUSTQUEUE_HOST")]
        host: String,
        /// Server HTTP port
        #[arg(long, default_value_t = 6790, env = "RUSTQUEUE_HTTP_PORT")]
        http_port: u16,
    },

    /// Manage schedules on a running server
    #[cfg(feature = "cli")]
    Schedules {
        #[command(subcommand)]
        action: ScheduleAction,
        /// Server host
        #[arg(long, default_value = "127.0.0.1", env = "RUSTQUEUE_HOST")]
        host: String,
        /// Server HTTP port
        #[arg(long, default_value_t = 6790, env = "RUSTQUEUE_HTTP_PORT")]
        http_port: u16,
    },
}

#[cfg(feature = "cli")]
#[derive(clap::Subcommand)]
enum ScheduleAction {
    /// List all schedules
    List,
    /// Create a new schedule
    Create {
        /// Schedule name (unique identifier)
        #[arg(long)]
        name: String,
        /// Target queue
        #[arg(long)]
        queue: String,
        /// Job name for created jobs
        #[arg(long)]
        job_name: String,
        /// Job data as JSON string
        #[arg(long, default_value = "{}")]
        data: String,
        /// Cron expression (e.g. "*/5 * * * *")
        #[arg(long)]
        cron: Option<String>,
        /// Interval in milliseconds
        #[arg(long)]
        every_ms: Option<u64>,
    },
    /// Delete a schedule
    Delete {
        /// Schedule name
        name: String,
    },
    /// Pause a schedule
    Pause {
        /// Schedule name
        name: String,
    },
    /// Resume a paused schedule
    Resume {
        /// Schedule name
        name: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Serve {
            config: config_path,
            http_port,
            tcp_port,
        } => {
            // 1. Load config from TOML file, fall back to defaults
            let mut config = match std::fs::read_to_string(&config_path) {
                Ok(contents) => {
                    toml::from_str::<rustqueue::config::RustQueueConfig>(&contents)?
                }
                Err(_) => {
                    rustqueue::config::RustQueueConfig::default()
                }
            };

            // 2. Initialize tracing based on config
            #[cfg(feature = "otel")]
            let otel_endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
                .unwrap_or_else(|_| "http://localhost:4317".to_string());

            #[cfg(feature = "otel")]
            {
                use tracing_subscriber::layer::SubscriberExt;
                use tracing_subscriber::util::SubscriberInitExt;

                let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| format!("rustqueue={}", config.logging.level).into());

                if config.logging.format == "json" {
                    let otel_layer = rustqueue::engine::telemetry::create_otel_layer(
                        "rustqueue",
                        &otel_endpoint,
                    )?;
                    tracing_subscriber::registry()
                        .with(env_filter)
                        .with(tracing_subscriber::fmt::layer().json())
                        .with(otel_layer)
                        .init();
                } else {
                    let otel_layer = rustqueue::engine::telemetry::create_otel_layer(
                        "rustqueue",
                        &otel_endpoint,
                    )?;
                    tracing_subscriber::registry()
                        .with(env_filter)
                        .with(tracing_subscriber::fmt::layer())
                        .with(otel_layer)
                        .init();
                }

                info!("OpenTelemetry enabled, exporting to {}", otel_endpoint);
            }

            #[cfg(not(feature = "otel"))]
            {
                let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| format!("rustqueue={}", config.logging.level).into());

                if config.logging.format == "json" {
                    tracing_subscriber::fmt()
                        .json()
                        .with_env_filter(env_filter)
                        .init();
                } else {
                    tracing_subscriber::fmt()
                        .with_env_filter(env_filter)
                        .init();
                }
            }

            info!(
                path = %config_path,
                format = %config.logging.format,
                "Tracing initialized"
            );

            // 2. Apply CLI overrides for ports
            if let Some(port) = http_port {
                config.server.http_port = port;
            }
            if let Some(port) = tcp_port {
                config.server.tcp_port = port;
            }

            // 3. Initialize storage backend based on config
            let storage: Arc<dyn rustqueue::storage::StorageBackend> = match config.storage.backend {
                rustqueue::config::StorageBackendType::Redb => {
                    std::fs::create_dir_all(&config.storage.path)?;
                    let db_path = std::path::Path::new(&config.storage.path).join("rustqueue.redb");
                    let s = Arc::new(rustqueue::storage::RedbStorage::new(&db_path)?);
                    info!(path = %db_path.display(), "RedbStorage initialized");
                    s
                }
                rustqueue::config::StorageBackendType::InMemory => {
                    let s = Arc::new(rustqueue::storage::MemoryStorage::new());
                    info!("InMemory storage initialized");
                    s
                }
                #[cfg(feature = "sqlite")]
                rustqueue::config::StorageBackendType::Sqlite => {
                    std::fs::create_dir_all(&config.storage.path)?;
                    let db_path = std::path::Path::new(&config.storage.path).join("rustqueue.db");
                    let s = Arc::new(rustqueue::storage::SqliteStorage::new(&db_path)?);
                    info!(path = %db_path.display(), "SqliteStorage initialized");
                    s
                }
                #[cfg(feature = "postgres")]
                rustqueue::config::StorageBackendType::Postgres => {
                    let url = config.storage.postgres_url.as_ref()
                        .ok_or_else(|| anyhow::anyhow!(
                            "PostgreSQL backend requires storage.postgres_url in config"
                        ))?;
                    let s = Arc::new(rustqueue::storage::PostgresStorage::new(url).await?);
                    info!("PostgresStorage initialized");
                    s
                }
                #[allow(unreachable_patterns)]
                other => {
                    anyhow::bail!("Storage backend '{other:?}' is not compiled in. Enable the corresponding feature flag.");
                }
            };

            // 4. Create broadcast channel for real-time job events
            let (event_tx, _) = tokio::sync::broadcast::channel(1024);

            // 5. Create QueueManager with storage and event sender
            let queue_manager = Arc::new(
                rustqueue::engine::queue::QueueManager::new(storage)
                    .with_event_sender(event_tx.clone()),
            );

            // 6. Install Prometheus metrics recorder
            let metrics_handle = metrics_exporter_prometheus::PrometheusBuilder::new()
                .install_recorder()
                .expect("failed to install Prometheus recorder");

            // 7. Build HTTP app state and router
            let state = Arc::new(rustqueue::api::AppState {
                queue_manager: Arc::clone(&queue_manager),
                start_time: std::time::Instant::now(),
                metrics_handle: Some(metrics_handle),
                event_tx: event_tx.clone(),
                auth_config: config.auth.clone(),
            });
            let app = rustqueue::api::router(state);

            // 8. Bind HTTP and TCP listeners
            let http_addr = format!("{}:{}", config.server.host, config.server.http_port);
            let tcp_addr = format!("{}:{}", config.server.host, config.server.tcp_port);

            let http_listener = tokio::net::TcpListener::bind(&http_addr).await?;
            let tcp_listener = tokio::net::TcpListener::bind(&tcp_addr).await?;

            info!(
                http = %http_addr,
                tcp = %tcp_addr,
                "RustQueue server starting"
            );

            // 9. Spawn background scheduler
            let scheduler_handle = rustqueue::engine::scheduler::start_scheduler(
                Arc::clone(&queue_manager),
                config.scheduler.tick_interval_ms,
                config.jobs.stall_timeout_ms,
                config.retention.clone(),
            );
            info!(
                tick_ms = config.scheduler.tick_interval_ms,
                stall_timeout_ms = config.jobs.stall_timeout_ms,
                "Background scheduler started"
            );

            // 10. Create shutdown signal
            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

            // 11. Spawn HTTP server with graceful shutdown
            let http_handle = tokio::spawn({
                let mut rx = shutdown_rx.clone();
                async move {
                    axum::serve(http_listener, app)
                        .with_graceful_shutdown(async move {
                            rx.changed().await.ok();
                        })
                        .await
                        .expect("HTTP server error");
                }
            });

            // 12. Spawn TCP server with graceful shutdown (auth config for connection-level authentication)
            let tcp_auth_config = config.auth.clone();

            // When TLS is enabled, start the TLS TCP server; otherwise start the plain TCP server.
            #[cfg(feature = "tls")]
            let tcp_handle = if config.tls.enabled {
                let certs = load_certs(&config.tls.cert_path)?;
                let key = load_key(&config.tls.key_path)?;
                let tls_config = rustls::ServerConfig::builder()
                    .with_no_client_auth()
                    .with_single_cert(certs, key)?;
                let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls_config));
                info!("TLS enabled for TCP protocol");
                tokio::spawn({
                    let rx = shutdown_rx.clone();
                    async move {
                        rustqueue::protocol::start_tls_tcp_server(
                            tcp_listener,
                            queue_manager,
                            tcp_auth_config,
                            rx,
                            acceptor,
                        )
                        .await;
                    }
                })
            } else {
                tokio::spawn({
                    let rx = shutdown_rx.clone();
                    async move {
                        rustqueue::protocol::start_tcp_server(
                            tcp_listener,
                            queue_manager,
                            tcp_auth_config,
                            rx,
                        )
                        .await;
                    }
                })
            };

            #[cfg(not(feature = "tls"))]
            let tcp_handle = tokio::spawn({
                let rx = shutdown_rx.clone();
                async move {
                    rustqueue::protocol::start_tcp_server(
                        tcp_listener,
                        queue_manager,
                        tcp_auth_config,
                        rx,
                    )
                    .await;
                }
            });

            // 13. Wait for shutdown signal (Ctrl+C)
            tokio::signal::ctrl_c().await?;
            info!("Shutdown signal received, draining connections...");
            shutdown_tx.send(true)?;

            // 14. Wait for servers with timeout (30s drain period)
            let http_abort = http_handle.abort_handle();
            let tcp_abort = tcp_handle.abort_handle();
            let drain_timeout = std::time::Duration::from_secs(30);
            match tokio::time::timeout(drain_timeout, async {
                let _ = http_handle.await;
                let _ = tcp_handle.await;
            })
            .await
            {
                Ok(_) => info!("All servers stopped gracefully"),
                Err(_) => {
                    warn!("Drain timeout reached, forcing shutdown");
                    http_abort.abort();
                    tcp_abort.abort();
                }
            }

            // Scheduler can be aborted immediately (safe, no in-flight state)
            scheduler_handle.abort();

            #[cfg(feature = "otel")]
            rustqueue::engine::telemetry::shutdown_otel();

            info!("RustQueue server stopped");
        }

        #[cfg(feature = "cli")]
        Commands::Status { host, http_port } => {
            let url = format!("http://{}:{}/api/v1/queues", host, http_port);
            let client = reqwest::Client::new();
            let resp = client.get(&url).send().await?;
            let body: serde_json::Value = resp.json().await?;

            if body["ok"].as_bool() == Some(true) {
                if let Some(queues) = body["queues"].as_array() {
                    if queues.is_empty() {
                        println!("No queues found.");
                    } else {
                        println!(
                            "{:<20} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
                            "Queue", "Waiting", "Active", "Delayed", "Done", "Failed", "DLQ"
                        );
                        println!("{}", "-".repeat(78));
                        for q in queues {
                            let name = q["name"].as_str().unwrap_or("?");
                            let c = &q["counts"];
                            println!(
                                "{:<20} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
                                name,
                                c["waiting"].as_u64().unwrap_or(0),
                                c["active"].as_u64().unwrap_or(0),
                                c["delayed"].as_u64().unwrap_or(0),
                                c["completed"].as_u64().unwrap_or(0),
                                c["failed"].as_u64().unwrap_or(0),
                                c["dlq"].as_u64().unwrap_or(0),
                            );
                        }
                    }
                }
            } else {
                eprintln!("Error: {}", body);
            }
        }

        #[cfg(feature = "cli")]
        Commands::Push {
            queue,
            name,
            data,
            host,
            http_port,
        } => {
            let url = format!(
                "http://{}:{}/api/v1/queues/{}/jobs",
                host, http_port, queue
            );
            let payload: serde_json::Value =
                serde_json::from_str(&data).unwrap_or_else(|_| serde_json::json!({}));
            let body = serde_json::json!({
                "name": name,
                "data": payload,
            });
            let client = reqwest::Client::new();
            let resp = client.post(&url).json(&body).send().await?;
            let result: serde_json::Value = resp.json().await?;
            if result["ok"].as_bool() == Some(true) {
                println!("Job pushed: {}", result["id"].as_str().unwrap_or("?"));
            } else {
                eprintln!("Error: {}", result);
            }
        }

        #[cfg(feature = "cli")]
        Commands::Inspect {
            id,
            host,
            http_port,
        } => {
            let url = format!("http://{}:{}/api/v1/jobs/{}", host, http_port, id);
            let client = reqwest::Client::new();
            let resp = client.get(&url).send().await?;
            let body: serde_json::Value = resp.json().await?;
            if body["ok"].as_bool() == Some(true) {
                println!("{}", serde_json::to_string_pretty(&body["job"])?);
            } else {
                eprintln!("Error: {}", body);
            }
        }

        #[cfg(feature = "cli")]
        Commands::Schedules { action, host, http_port } => {
            let base = format!("http://{}:{}/api/v1/schedules", host, http_port);
            let client = reqwest::Client::new();

            match action {
                ScheduleAction::List => {
                    let resp = client.get(&base).send().await?;
                    let body: serde_json::Value = resp.json().await?;
                    if body["ok"].as_bool() == Some(true) {
                        if let Some(schedules) = body["schedules"].as_array() {
                            if schedules.is_empty() {
                                println!("No schedules found.");
                            } else {
                                println!("{:<25} {:<15} {:<25} {:>6} {:<7}", "Name", "Queue", "Timing", "Runs", "Paused");
                                println!("{}", "-".repeat(80));
                                for s in schedules {
                                    let name = s["name"].as_str().unwrap_or("?");
                                    let queue = s["queue"].as_str().unwrap_or("?");
                                    let timing = if let Some(cron) = s["cron_expr"].as_str() {
                                        format!("cron: {}", cron)
                                    } else if let Some(ms) = s["every_ms"].as_u64() {
                                        format!("every {}ms", ms)
                                    } else {
                                        "?".to_string()
                                    };
                                    let runs = s["execution_count"].as_u64().unwrap_or(0);
                                    let paused = s["paused"].as_bool().unwrap_or(false);
                                    println!("{:<25} {:<15} {:<25} {:>6} {:<7}", name, queue, timing, runs, paused);
                                }
                            }
                        }
                    } else {
                        eprintln!("Error: {}", body);
                    }
                }
                ScheduleAction::Create { name, queue, job_name, data, cron, every_ms } => {
                    let payload: serde_json::Value = serde_json::from_str(&data).unwrap_or(serde_json::json!({}));
                    let mut body = serde_json::json!({
                        "name": name,
                        "queue": queue,
                        "job_name": job_name,
                        "job_data": payload,
                    });
                    if let Some(c) = cron { body["cron_expr"] = serde_json::json!(c); }
                    if let Some(ms) = every_ms { body["every_ms"] = serde_json::json!(ms); }
                    let resp = client.post(&base).json(&body).send().await?;
                    let result: serde_json::Value = resp.json().await?;
                    if result["ok"].as_bool() == Some(true) {
                        println!("Schedule '{}' created.", name);
                    } else {
                        eprintln!("Error: {}", result);
                    }
                }
                ScheduleAction::Delete { name } => {
                    let url = format!("{}/{}", base, name);
                    let resp = client.delete(&url).send().await?;
                    let result: serde_json::Value = resp.json().await?;
                    if result["ok"].as_bool() == Some(true) {
                        println!("Schedule '{}' deleted.", name);
                    } else {
                        eprintln!("Error: {}", result);
                    }
                }
                ScheduleAction::Pause { name } => {
                    let url = format!("{}/{}/pause", base, name);
                    let resp = client.post(&url).send().await?;
                    let result: serde_json::Value = resp.json().await?;
                    if result["ok"].as_bool() == Some(true) {
                        println!("Schedule '{}' paused.", name);
                    } else {
                        eprintln!("Error: {}", result);
                    }
                }
                ScheduleAction::Resume { name } => {
                    let url = format!("{}/{}/resume", base, name);
                    let resp = client.post(&url).send().await?;
                    let result: serde_json::Value = resp.json().await?;
                    if result["ok"].as_bool() == Some(true) {
                        println!("Schedule '{}' resumed.", name);
                    } else {
                        eprintln!("Error: {}", result);
                    }
                }
            }
        }
    }

    Ok(())
}
