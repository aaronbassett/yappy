//! TTS Provider trait and metadata types

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::audio::{AudioFormat, AudioStream};
use crate::error::ProviderError;
use crate::session::VoiceConfig;

/// Unique provider identifier
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProviderId(pub String);

impl ProviderId {
    /// Create a new provider ID
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl std::fmt::Display for ProviderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// TTS Provider trait - implemented by each backend
#[async_trait]
pub trait TtsProvider: Send + Sync {
    /// Get provider metadata
    fn metadata(&self) -> ProviderMetadata;

    /// Check if provider is ready to accept requests
    async fn health_check(&self) -> ProviderStatus;

    /// Synthesize text to audio stream
    ///
    /// Returns a stream of audio chunks. The provider should check the
    /// cancellation token periodically and stop work if cancelled.
    async fn synthesize(
        &self,
        text: &str,
        voice: &VoiceConfig,
        format: AudioFormat,
        cancel: CancellationToken,
    ) -> Result<AudioStream, ProviderError>;
}

/// Provider metadata exposed via /providers endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderMetadata {
    /// Unique identifier (e.g., "kokoro", "openai", "avspeech")
    pub id: ProviderId,

    /// Human-readable name
    pub name: String,

    /// Description of the provider
    pub description: String,

    /// Available voices
    pub voices: Vec<VoiceInfo>,

    /// Supported audio output formats
    pub supported_formats: Vec<AudioFormat>,

    /// Provider-specific options schema (JSON Schema)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options_schema: Option<serde_json::Value>,
}

/// Provider readiness status
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProviderStatus {
    /// Ready to accept synthesis requests
    Available,

    /// Provider is compiled in but not configured
    NotConfigured {
        /// Human-readable reason
        reason: String,
    },

    /// Provider is temporarily unavailable
    Unavailable {
        /// Human-readable reason
        reason: String,
    },
}

impl ProviderStatus {
    /// Check if provider is available
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }
}

/// Voice information for discovery
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceInfo {
    /// Voice identifier (provider-specific)
    pub id: String,

    /// Human-readable name
    pub name: String,

    /// Language code (BCP 47, e.g., "en-US")
    pub language: String,

    /// Voice gender if applicable
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gender: Option<VoiceGender>,

    /// Sample audio URL if available
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_url: Option<String>,
}

/// Voice gender
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VoiceGender {
    /// Male voice
    Male,
    /// Female voice
    Female,
    /// Gender-neutral voice
    Neutral,
}
