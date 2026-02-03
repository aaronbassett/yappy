//! Yappy Streaming TTS Server
//!
//! A standalone server that accepts streaming text input via WebSocket
//! and produces near-realtime audio output through pluggable TTS providers.

pub mod app;
mod state;
pub mod ws;

pub use app::create_router;
pub use state::{AppState, ProviderRegistry};
pub use ws::ws_upgrade_handler;

use clap::Parser;
use std::path::PathBuf;
use tracing::info;

/// Yappy Streaming TTS Server
#[derive(Parser, Debug)]
#[command(name = "yappy-server")]
#[command(about = "Streaming text-to-speech server")]
#[command(version)]
struct Args {
    /// Path to configuration file
    #[arg(short, long, default_value = "yappy.toml")]
    config: PathBuf,

    /// Override bind address
    #[arg(long)]
    host: Option<String>,

    /// Override port
    #[arg(short, long)]
    port: Option<u16>,

    /// Log level (error, warn, info, debug, trace)
    #[arg(long, env = "RUST_LOG")]
    log_level: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Initialize tracing
    let log_level = args.log_level.as_deref().unwrap_or("info");
    tracing_subscriber::fmt()
        .with_env_filter(log_level)
        .with_target(true)
        .init();

    info!("Starting Yappy TTS Server");
    info!("Config file: {}", args.config.display());

    // Load configuration
    let config = yappy_core::Config::load(&args.config)?;

    // Apply CLI overrides
    let host = args.host.unwrap_or(config.server.host);
    let port = args.port.unwrap_or(config.server.port);
    let bind_addr = format!("{host}:{port}");

    info!("Binding to {}", bind_addr);

    // TODO: Initialize providers based on config and features
    // TODO: Create Axum router with /health, /providers, /ws endpoints
    // TODO: Start server

    info!("Server implementation pending - see specs/001-streaming-tts-server/");

    Ok(())
}
