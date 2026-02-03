//! Yappy Streaming TTS Server Library
//!
//! A standalone server that accepts streaming text input via WebSocket
//! and produces near-realtime audio output through pluggable TTS providers.
//!
//! This library crate exposes the core server components for testing and reuse.

pub mod app;
pub mod state;
pub mod ws;

pub use app::create_router;
pub use state::{AppState, ProviderRegistry};
pub use ws::ws_upgrade_handler;
