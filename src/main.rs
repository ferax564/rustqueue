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

        /// HTTP port
        #[arg(long, env = "RUSTQUEUE_HTTP_PORT", default_value = "6790")]
        http_port: u16,

        /// TCP port
        #[arg(long, env = "RUSTQUEUE_TCP_PORT", default_value = "6789")]
        tcp_port: u16,
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
            config: _config_path,
            http_port,
            tcp_port,
        } => {
            info!(http_port, tcp_port, "Starting RustQueue server");
            // Server startup will be implemented in Phase 1
            info!("RustQueue is not yet implemented. See docs/plans/ for the implementation plan.");
        }
    }

    Ok(())
}
