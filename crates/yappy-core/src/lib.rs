//! Yappy Core - Shared types and traits for the TTS server
//!
//! This crate provides the core abstractions used across Yappy:
//! - `TtsProvider` trait for implementing TTS backends
//! - Session, audio, and configuration types
//! - Error types
//! - WebSocket message types

#![warn(missing_docs)]
#![warn(clippy::all)]

/// TTS provider trait and related types
pub mod provider;

/// Session management types
pub mod session;

/// Audio format and chunk types
pub mod audio;

/// Sentence buffer implementation
pub mod buffer;

/// Configuration types
pub mod config;

/// Error types
pub mod error;

/// WebSocket message types
pub mod message;

/// Audio transcoding module
pub mod transcode;

// Re-exports for convenience
pub use audio::{AudioChunk, AudioCodec, AudioFormat};
pub use buffer::SentenceBuffer;
pub use config::{Config, ConfigValidationError};
pub use error::{ErrorResponse, ProviderError, SessionError};
pub use message::{ClientMessage, ServerMessage};
pub use provider::{ProviderMetadata, ProviderStatus, TtsProvider};
pub use session::{CodeBlockMode, Session, SessionId, SessionState, VoiceConfig};
pub use transcode::{
    f32_to_i16, i16_to_bytes_le, Mp3Encoder, Mp3Quality, OpusEncoder, TranscodeCapability,
    TranscodeError, Transcoder,
};
