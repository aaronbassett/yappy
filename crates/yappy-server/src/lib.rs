//! Yappy Streaming TTS Server Library
//!
//! A standalone server that accepts streaming text input via WebSocket
//! and produces near-realtime audio output through pluggable TTS providers.
//!
//! This library crate exposes the core server components for testing and reuse.

pub mod app;
pub mod handlers;
pub mod shutdown;
pub mod state;
pub mod ws;

pub use app::create_router;
pub use handlers::list_providers;
pub use shutdown::{SessionGuard, ShutdownCoordinator};
pub use state::{AppState, ProviderRegistry};
pub use ws::ws_upgrade_handler;
