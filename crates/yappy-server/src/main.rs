//! Yappy Streaming TTS Server
//!
//! A standalone server that accepts streaming text input via WebSocket
//! and produces near-realtime audio output through pluggable TTS providers.

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use tokio::net::TcpListener;
use tracing::{error, info, warn};

use yappy_server::create_router;
use yappy_server::shutdown::{ShutdownCoordinator, DEFAULT_DRAIN_TIMEOUT};
use yappy_server::state::{register_providers, AppState};

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
    let host = args.host.unwrap_or_else(|| config.server.host.clone());
    let port = args.port.unwrap_or(config.server.port);
    let bind_addr = format!("{host}:{port}");

    // Initialize TTS providers based on config and feature flags
    info!("Initializing TTS providers...");
    let registry = register_providers(&config).await;

    if registry.is_empty() {
        warn!("No TTS providers registered - server will have limited functionality");
    } else {
        info!(
            providers = registry.len(),
            default = ?registry.default_provider().map(|p| p.0),
            "TTS providers initialized"
        );
    }

    // Create the shutdown coordinator for graceful shutdown (SC-007)
    let shutdown_coordinator = Arc::new(ShutdownCoordinator::new());

    // Create application state with the shutdown coordinator
    let state =
        AppState::with_shutdown_coordinator(config, registry, Arc::clone(&shutdown_coordinator));

    // Create Axum router with /health, /providers, /ws endpoints
    let router = create_router(state);

    // Bind to the configured address
    let listener = TcpListener::bind(&bind_addr).await?;
    info!("Server listening on {}", bind_addr);

    // Set up signal handlers for graceful shutdown (FR-027, SC-007)
    let shutdown_signal = shutdown_signal(Arc::clone(&shutdown_coordinator));

    // Start the server with graceful shutdown support
    info!("Server ready to accept connections");
    if let Err(e) = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal)
        .await
    {
        error!(error = %e, "Server error");
        return Err(e.into());
    }

    // Wait for active sessions to drain with timeout (SC-007: 5 second timeout)
    info!("Server stopped accepting connections, waiting for active sessions to drain...");
    let drained = shutdown_coordinator
        .wait_for_drain(DEFAULT_DRAIN_TIMEOUT)
        .await;

    if drained {
        info!("All sessions completed gracefully");
    } else {
        warn!(
            remaining_sessions = shutdown_coordinator.active_session_count(),
            "Drain timeout expired, some sessions may have been terminated"
        );
    }

    info!("Server shutdown complete");
    Ok(())
}

/// Create a future that completes when a shutdown signal is received.
///
/// Handles both SIGTERM (Unix) and Ctrl+C (cross-platform) signals.
/// When a signal is received, initiates graceful shutdown by notifying
/// the shutdown coordinator.
///
/// # Arguments
///
/// * `coordinator` - The shutdown coordinator to notify when a signal is received
async fn shutdown_signal(coordinator: Arc<ShutdownCoordinator>) {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("Failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {
            info!("Received Ctrl+C (SIGINT), initiating graceful shutdown");
        }
        () = terminate => {
            info!("Received SIGTERM, initiating graceful shutdown");
        }
    }

    // Initiate graceful shutdown - this cancels the token that all sessions monitor
    coordinator.initiate_shutdown();
}
