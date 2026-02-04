//! Session management types

use serde::{Deserialize, Serialize};
use std::time::Instant;
use uuid::Uuid;

use crate::audio::AudioFormat;
use crate::buffer::{BufferConfig, SentenceBuffer};
use crate::provider::ProviderId;

/// Unique session identifier
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(String);

impl SessionId {
    /// Create a new random session ID
    pub fn new() -> Self {
        Self(format!("ses_{}", Uuid::new_v4().simple()))
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Session state for an active WebSocket connection.
///
/// # Isolation (FR-018)
///
/// Each `Session` instance is fully isolated from other sessions:
///
/// - All fields are **owned** (no `Arc`, `Rc`, or shared references to mutable data)
/// - The [`SentenceBuffer`] is owned directly, maintaining independent buffer state
/// - Voice configuration, audio format, and code block mode are per-session
/// - Statistics (`total_duration_ms`, `total_bytes`, `audio_sequence`) are per-session
///
/// This design ensures no cross-contamination between concurrent WebSocket connections.
/// The server creates a new `Session` for each connection, and all session state is
/// local to that connection's handler task.
pub struct Session {
    /// Unique identifier for this session
    pub id: SessionId,

    /// Selected TTS provider for this session
    pub provider_id: ProviderId,

    /// Selected voice configuration
    pub voice: VoiceConfig,

    /// Negotiated audio output format
    pub audio_format: AudioFormat,

    /// Sentence buffer for text accumulation
    pub buffer: SentenceBuffer,

    /// Session creation timestamp
    pub created_at: Instant,

    /// Last activity timestamp (for idle timeout)
    pub last_activity: Instant,

    /// Current session state
    pub state: SessionState,

    /// Code block handling mode
    pub code_block_mode: CodeBlockMode,

    /// Cumulative audio duration in milliseconds (for audio.done statistics)
    pub total_duration_ms: u64,

    /// Cumulative audio bytes sent (for audio.done statistics)
    pub total_bytes: u64,

    /// Current audio chunk sequence number
    pub audio_sequence: u32,
}

impl Session {
    /// Create a new session
    pub fn new(
        provider_id: ProviderId,
        voice: VoiceConfig,
        audio_format: AudioFormat,
        code_block_mode: CodeBlockMode,
    ) -> Self {
        let now = Instant::now();
        Self {
            id: SessionId::new(),
            provider_id,
            voice,
            audio_format,
            buffer: SentenceBuffer::default(),
            created_at: now,
            last_activity: now,
            state: SessionState::Ready,
            code_block_mode,
            total_duration_ms: 0,
            total_bytes: 0,
            audio_sequence: 0,
        }
    }

    /// Record audio chunk statistics and increment sequence number
    ///
    /// Returns the sequence number that was assigned to this chunk.
    pub fn record_audio_chunk(&mut self, duration_ms: u32, bytes: usize) -> u32 {
        let seq = self.audio_sequence;
        self.audio_sequence += 1;
        self.total_duration_ms += u64::from(duration_ms);
        self.total_bytes += bytes as u64;
        seq
    }

    /// Update last activity timestamp
    pub fn touch(&mut self) {
        self.last_activity = Instant::now();
    }

    /// Create a new session with custom buffer configuration
    pub fn with_buffer_config(
        provider_id: ProviderId,
        voice: VoiceConfig,
        audio_format: AudioFormat,
        code_block_mode: CodeBlockMode,
        buffer_config: BufferConfig,
    ) -> Self {
        let now = Instant::now();
        Self {
            id: SessionId::new(),
            provider_id,
            voice,
            audio_format,
            buffer: SentenceBuffer::new(buffer_config),
            created_at: now,
            last_activity: now,
            state: SessionState::Ready,
            code_block_mode,
            total_duration_ms: 0,
            total_bytes: 0,
            audio_sequence: 0,
        }
    }
}

/// Session lifecycle states
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionState {
    /// Session initialized, waiting for text
    #[default]
    Ready,
    /// Actively processing text and streaming audio
    Streaming,
    /// Client sent text.done, flushing remaining audio
    Completing,
    /// Session ended (normal or error)
    Closed,
}

/// How to handle fenced code blocks in input text
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeBlockMode {
    /// Skip code blocks entirely (default)
    #[default]
    Skip,
    /// Read code blocks literally
    ReadLiterally,
    /// Announce "code block" then skip content
    AnnounceAndSkip,
}

/// Voice configuration for synthesis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceConfig {
    /// Voice identifier (empty string means use provider default)
    #[serde(default)]
    pub id: String,

    /// Speech rate multiplier (0.5 - 2.0, default 1.0)
    #[serde(default = "default_speed")]
    pub speed: f32,

    /// Pitch adjustment if supported (-1.0 to 1.0, default 0.0)
    #[serde(default)]
    pub pitch: f32,

    /// Volume adjustment (0.0 to 1.0, default 1.0)
    #[serde(default = "default_volume")]
    pub volume: f32,
}

const fn default_speed() -> f32 {
    1.0
}

const fn default_volume() -> f32 {
    1.0
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            speed: 1.0,
            pitch: 0.0,
            volume: 1.0,
        }
    }
}

impl VoiceConfig {
    /// Validate voice configuration
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(0.5..=2.0).contains(&self.speed) {
            return Err("speed must be between 0.5 and 2.0");
        }
        if !(-1.0..=1.0).contains(&self.pitch) {
            return Err("pitch must be between -1.0 and 1.0");
        }
        if !(0.0..=1.0).contains(&self.volume) {
            return Err("volume must be between 0.0 and 1.0");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that VoiceConfig can be deserialized with only speed specified (protocol contract)
    #[test]
    fn test_voice_config_partial_speed_only() {
        let json = r#"{"speed": 1.5}"#;
        let config: VoiceConfig = serde_json::from_str(json).unwrap();
        assert!(config.id.is_empty()); // default empty string
        assert!((config.speed - 1.5).abs() < f32::EPSILON);
        assert!((config.pitch - 0.0).abs() < f32::EPSILON); // default
        assert!((config.volume - 1.0).abs() < f32::EPSILON); // default
    }

    /// Test that VoiceConfig can be deserialized with only pitch specified
    #[test]
    fn test_voice_config_partial_pitch_only() {
        let json = r#"{"pitch": 0.5}"#;
        let config: VoiceConfig = serde_json::from_str(json).unwrap();
        assert!(config.id.is_empty()); // default
        assert!((config.speed - 1.0).abs() < f32::EPSILON); // default
        assert!((config.pitch - 0.5).abs() < f32::EPSILON);
        assert!((config.volume - 1.0).abs() < f32::EPSILON); // default
    }

    /// Test that empty VoiceConfig object deserializes with all defaults
    #[test]
    fn test_voice_config_empty_object() {
        let json = r#"{}"#;
        let config: VoiceConfig = serde_json::from_str(json).unwrap();
        assert!(config.id.is_empty());
        assert!((config.speed - 1.0).abs() < f32::EPSILON);
        assert!((config.pitch - 0.0).abs() < f32::EPSILON);
        assert!((config.volume - 1.0).abs() < f32::EPSILON);
    }

    /// Test that VoiceConfig works with mixed specified and default fields
    #[test]
    fn test_voice_config_partial_mixed() {
        let json = r#"{"id": "af_bella", "volume": 0.8}"#;
        let config: VoiceConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.id, "af_bella");
        assert!((config.speed - 1.0).abs() < f32::EPSILON); // default
        assert!((config.pitch - 0.0).abs() < f32::EPSILON); // default
        assert!((config.volume - 0.8).abs() < f32::EPSILON);
    }

    /// Test that VoiceConfig validation passes for defaults
    #[test]
    fn test_voice_config_default_validates() {
        let config = VoiceConfig::default();
        assert!(config.validate().is_ok());
    }

    /// Test that VoiceConfig validation fails for out-of-range speed
    #[test]
    fn test_voice_config_validation_speed_out_of_range() {
        let config = VoiceConfig {
            speed: 3.0,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }
}
