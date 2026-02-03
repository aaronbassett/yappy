//! Kokoro ONNX TTS Provider
//!
//! Local TTS using the Kokoro 82M ONNX model.
//! Downloads model from `HuggingFace` on first use.
//!
//! # Overview
//!
//! This provider implements text-to-speech synthesis using the Kokoro 82M model,
//! running inference locally via ONNX Runtime. The model is automatically downloaded
//! from `HuggingFace` Hub when not present locally.
//!
//! # Example
//!
//! ```ignore
//! use yappy_provider_kokoro::{KokoroProvider, KokoroConfig};
//!
//! // Create with default configuration
//! let provider = KokoroProvider::with_defaults();
//!
//! // Or with custom configuration
//! let config = KokoroConfig {
//!     model_path: Some("/path/to/model.onnx".into()),
//!     default_voice: "af_heart".to_string(),
//! };
//! let provider = KokoroProvider::new(config);
//! ```

#![warn(missing_docs)]

use std::path::PathBuf;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use yappy_core::audio::{AudioFormat, AudioStream};
use yappy_core::error::ProviderError;
use yappy_core::provider::{ProviderId, ProviderMetadata, ProviderStatus};
use yappy_core::session::VoiceConfig;
use yappy_core::TtsProvider;

/// Default voice ID for Kokoro model
const DEFAULT_VOICE_ID: &str = "af_heart";

/// Configuration for the Kokoro TTS provider
#[derive(Debug, Clone)]
pub struct KokoroConfig {
    /// Path to the ONNX model file.
    ///
    /// If `None`, the model will be automatically downloaded from `HuggingFace`
    /// Hub on first use and cached locally.
    pub model_path: Option<PathBuf>,

    /// Default voice ID to use when none is specified.
    ///
    /// Kokoro supports multiple voice styles. See the voice list
    /// documentation for available options.
    pub default_voice: String,
}

impl Default for KokoroConfig {
    fn default() -> Self {
        Self {
            model_path: None,
            default_voice: DEFAULT_VOICE_ID.to_string(),
        }
    }
}

/// Kokoro 82M ONNX TTS Provider
///
/// A local TTS provider using the Kokoro 82M model with ONNX Runtime inference.
/// Designed for low-latency, offline text-to-speech synthesis.
///
/// # Features
///
/// - Runs entirely locally without network requests (after model download)
/// - Multiple voice styles supported
/// - Optimized for real-time streaming synthesis
///
/// # Status
///
/// This is a skeleton implementation. The ONNX session loading and
/// audio synthesis will be implemented in subsequent tasks.
pub struct KokoroProvider {
    /// Provider configuration
    config: KokoroConfig,
    // TODO: Add ONNX session field when implementing model loading (T126)
    // session: Option<ort::Session>,
}

impl KokoroProvider {
    /// Create a new Kokoro provider with the given configuration.
    ///
    /// Note: This does not load the ONNX model. The model will be loaded
    /// lazily on first synthesis request (not yet implemented).
    ///
    /// # Arguments
    ///
    /// * `config` - Provider configuration including model path and default voice
    ///
    /// # Example
    ///
    /// ```
    /// use yappy_provider_kokoro::{KokoroProvider, KokoroConfig};
    ///
    /// let config = KokoroConfig {
    ///     model_path: Some("/models/kokoro-v0.19.onnx".into()),
    ///     default_voice: "af_heart".to_string(),
    /// };
    /// let provider = KokoroProvider::new(config);
    /// ```
    pub const fn new(config: KokoroConfig) -> Self {
        Self { config }
    }

    /// Create a Kokoro provider with default configuration.
    ///
    /// Uses the default voice (`af_heart`) and will download the model
    /// from `HuggingFace` Hub when needed.
    ///
    /// # Example
    ///
    /// ```
    /// use yappy_provider_kokoro::KokoroProvider;
    ///
    /// let provider = KokoroProvider::with_defaults();
    /// ```
    pub fn with_defaults() -> Self {
        Self::new(KokoroConfig::default())
    }

    /// Get a reference to the provider configuration.
    pub const fn config(&self) -> &KokoroConfig {
        &self.config
    }

    // TODO: Implement model loading (T126)
    // /// Load the ONNX model session.
    // ///
    // /// Downloads from HuggingFace if no local path is configured.
    // pub async fn load_model(&mut self) -> Result<(), ProviderError> {
    //     unimplemented!()
    // }
}

#[async_trait]
impl TtsProvider for KokoroProvider {
    /// Returns metadata about the Kokoro provider.
    ///
    /// Includes provider identification, description, and supported formats.
    /// Voice list will be populated in a future implementation.
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            id: ProviderId::new("kokoro"),
            name: "Kokoro 82M".to_string(),
            description: "Local TTS using Kokoro ONNX model".to_string(),
            // TODO: Populate voices when implementing voice enumeration (T126)
            voices: Vec::new(),
            supported_formats: vec![AudioFormat::default()],
            options_schema: None,
        }
    }

    /// Check if the provider is ready to accept synthesis requests.
    ///
    /// Currently returns `NotConfigured` as the ONNX session loading
    /// is not yet implemented.
    async fn health_check(&self) -> ProviderStatus {
        // TODO: Check if ONNX session is loaded and ready (T126)
        ProviderStatus::NotConfigured {
            reason: "ONNX session not yet loaded".to_string(),
        }
    }

    /// Synthesize text to audio.
    ///
    /// Not yet implemented - returns an error indicating the provider
    /// is not ready for synthesis.
    ///
    /// # Arguments
    ///
    /// * `text` - The text to synthesize
    /// * `voice` - Voice configuration (id, speed, pitch, volume)
    /// * `format` - Desired audio output format
    /// * `cancel` - Cancellation token to stop synthesis early
    ///
    /// # Errors
    ///
    /// Currently always returns `ProviderError::SynthesisFailed` as
    /// synthesis is not yet implemented.
    async fn synthesize(
        &self,
        _text: &str,
        _voice: &VoiceConfig,
        _format: AudioFormat,
        _cancel: CancellationToken,
    ) -> Result<AudioStream, ProviderError> {
        // TODO: Implement ONNX inference and audio encoding (T127)
        Err(ProviderError::SynthesisFailed {
            message: "Not yet implemented".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = KokoroConfig::default();
        assert!(config.model_path.is_none());
        assert_eq!(config.default_voice, "af_heart");
    }

    #[test]
    fn test_with_defaults() {
        let provider = KokoroProvider::with_defaults();
        assert!(provider.config().model_path.is_none());
        assert_eq!(provider.config().default_voice, "af_heart");
    }

    #[test]
    fn test_custom_config() {
        let config = KokoroConfig {
            model_path: Some(PathBuf::from("/custom/path/model.onnx")),
            default_voice: "custom_voice".to_string(),
        };
        let provider = KokoroProvider::new(config);

        assert_eq!(
            provider.config().model_path,
            Some(PathBuf::from("/custom/path/model.onnx"))
        );
        assert_eq!(provider.config().default_voice, "custom_voice");
    }

    #[test]
    fn test_metadata() {
        let provider = KokoroProvider::with_defaults();
        let metadata = provider.metadata();

        assert_eq!(metadata.id.0, "kokoro");
        assert_eq!(metadata.name, "Kokoro 82M");
        assert_eq!(metadata.description, "Local TTS using Kokoro ONNX model");
        assert!(metadata.voices.is_empty());
        assert!(!metadata.supported_formats.is_empty());
    }

    #[tokio::test]
    async fn test_health_check_not_configured() {
        let provider = KokoroProvider::with_defaults();
        let status = provider.health_check().await;

        match status {
            ProviderStatus::NotConfigured { reason } => {
                assert!(reason.contains("ONNX session not yet loaded"));
            }
            _ => panic!("Expected NotConfigured status"),
        }
    }

    #[tokio::test]
    async fn test_synthesize_not_implemented() {
        let provider = KokoroProvider::with_defaults();
        let voice = VoiceConfig::default();
        let format = AudioFormat::default();
        let cancel = CancellationToken::new();

        let result = provider.synthesize("Hello", &voice, format, cancel).await;

        assert!(result.is_err());
        match result {
            Err(ProviderError::SynthesisFailed { message }) => {
                assert!(message.contains("Not yet implemented"));
            }
            _ => panic!("Expected SynthesisFailed error"),
        }
    }
}
