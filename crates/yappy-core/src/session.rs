//! Session management types

use serde::{Deserialize, Serialize};
use std::time::Instant;
use uuid::Uuid;

use crate::audio::AudioFormat;
use crate::buffer::SentenceBuffer;
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

/// Session state for an active WebSocket connection
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
        }
    }

    /// Update last activity timestamp
    pub fn touch(&mut self) {
        self.last_activity = Instant::now();
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
    /// Voice identifier
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
