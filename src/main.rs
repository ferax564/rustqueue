use std::sync::Arc;

use clap::Parser;
use tracing::info;

mod dashboard;

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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rustqueue=info".into()),
        )
        .init();

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
                    info!(path = %config_path, "Loaded configuration file");
                    toml::from_str::<rustqueue::config::RustQueueConfig>(&contents)?
                }
                Err(_) => {
                    info!(
                        path = %config_path,
                        "Config file not found, using defaults"
                    );
                    rustqueue::config::RustQueueConfig::default()
                }
            };

            // 2. Apply CLI overrides for ports
            if let Some(port) = http_port {
                config.server.http_port = port;
            }
            if let Some(port) = tcp_port {
                config.server.tcp_port = port;
            }

            // 3. Initialize RedbStorage at configured path
            std::fs::create_dir_all(&config.storage.path)?;
            let db_path = std::path::Path::new(&config.storage.path).join("rustqueue.redb");
            let storage = Arc::new(rustqueue::storage::RedbStorage::new(&db_path)?);
            info!(path = %db_path.display(), "Storage initialized");

            // 4. Create QueueManager with storage
            let queue_manager = Arc::new(rustqueue::engine::queue::QueueManager::new(storage));

            // 5. Install Prometheus metrics recorder
            let metrics_handle = metrics_exporter_prometheus::PrometheusBuilder::new()
                .install_recorder()
                .expect("failed to install Prometheus recorder");

            // 6. Build HTTP app state and router
            let state = Arc::new(rustqueue::api::AppState {
                queue_manager: Arc::clone(&queue_manager),
                start_time: std::time::Instant::now(),
                metrics_handle: Some(metrics_handle),
            });
            let app = rustqueue::api::router(state);

            // 7. Bind HTTP and TCP listeners
            let http_addr = format!("{}:{}", config.server.host, config.server.http_port);
            let tcp_addr = format!("{}:{}", config.server.host, config.server.tcp_port);

            let http_listener = tokio::net::TcpListener::bind(&http_addr).await?;
            let tcp_listener = tokio::net::TcpListener::bind(&tcp_addr).await?;

            info!(
                http = %http_addr,
                tcp = %tcp_addr,
                "RustQueue server starting"
            );

            // 8. Spawn HTTP server
            let http_handle = tokio::spawn(async move {
                axum::serve(http_listener, app)
                    .await
                    .expect("HTTP server error");
            });

            // 9. Spawn TCP server
            let tcp_handle = tokio::spawn(async move {
                rustqueue::protocol::start_tcp_server(tcp_listener, queue_manager).await;
            });

            // 10. Wait for shutdown signal (Ctrl+C)
            tokio::signal::ctrl_c().await?;
            info!("Shutdown signal received, stopping servers...");

            // Abort server tasks for clean shutdown
            http_handle.abort();
            tcp_handle.abort();

            info!("RustQueue server stopped");
        }
    }

    Ok(())
}
