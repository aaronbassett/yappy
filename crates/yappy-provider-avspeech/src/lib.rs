//! macOS `AVSpeechSynthesizer` Provider
//!
//! Native macOS TTS using `AVFoundation`'s `AVSpeechSynthesizer`.
//! Only available on macOS targets.
//!
//! # Status
//!
//! This provider is a work-in-progress. The implementation requires testing
//! and debugging on macOS to properly integrate with objc2-avf-audio APIs.
//!
//! # Features (when complete)
//!
//! - Native macOS speech synthesis using system voices
//! - Streaming audio output via buffer callbacks (macOS 10.15+)
//! - Access to all system-installed voices
//! - Configurable speech rate, pitch, and volume
//! - Cancellation support for mid-synthesis abort

#![warn(missing_docs)]
#![cfg(target_os = "macos")]

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use yappy_core::audio::{AudioCodec, AudioFormat, AudioStream};
use yappy_core::error::ProviderError;
use yappy_core::provider::{ProviderId, ProviderMetadata, ProviderStatus, TtsProvider, VoiceInfo};
use yappy_core::session::VoiceConfig;

/// Default voice identifier for macOS
const DEFAULT_VOICE_ID: &str = "com.apple.voice.compact.en-US.Samantha";

/// AVSpeech sample rate
const SAMPLE_RATE: u32 = 22050;

/// Configuration for the AVSpeech TTS provider
#[derive(Debug, Clone)]
pub struct AvSpeechConfig {
    /// Default voice identifier to use when none is specified.
    ///
    /// Should be a valid system voice identifier like
    /// `"com.apple.voice.compact.en-US.Samantha"`.
    pub default_voice: String,
}

impl Default for AvSpeechConfig {
    fn default() -> Self {
        Self {
            default_voice: DEFAULT_VOICE_ID.to_string(),
        }
    }
}

/// macOS AVSpeechSynthesizer TTS Provider
///
/// Uses Apple's AVFoundation framework for native text-to-speech synthesis.
///
/// # Note
///
/// This provider is currently a stub implementation. Full functionality
/// requires testing on macOS to properly integrate with objc2-avf-audio APIs.
pub struct AvSpeechProvider {
    /// Provider configuration
    config: AvSpeechConfig,
}

impl AvSpeechProvider {
    /// Create a new AVSpeech provider with the given configuration.
    ///
    /// # Arguments
    ///
    /// * `config` - Provider configuration including default voice
    pub fn new(config: AvSpeechConfig) -> Self {
        debug!(
            default_voice = %config.default_voice,
            "Creating AvSpeechProvider"
        );
        Self { config }
    }

    /// Create a provider with default configuration.
    ///
    /// Uses `"com.apple.voice.compact.en-US.Samantha"` as the default voice.
    pub fn with_defaults() -> Self {
        Self::new(AvSpeechConfig::default())
    }

    /// Get a reference to the provider configuration.
    pub const fn config(&self) -> &AvSpeechConfig {
        &self.config
    }
}

impl Default for AvSpeechProvider {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[async_trait]
impl TtsProvider for AvSpeechProvider {
    fn metadata(&self) -> ProviderMetadata {
        // Return a list of common macOS voices
        // In a full implementation, this would query AVSpeechSynthesisVoice.speechVoices()
        let voices = vec![
            VoiceInfo {
                id: "com.apple.voice.compact.en-US.Samantha".to_string(),
                name: "Samantha (US)".to_string(),
                language: "en-US".to_string(),
                gender: Some(yappy_core::provider::VoiceGender::Female),
                sample_url: None,
            },
            VoiceInfo {
                id: "com.apple.voice.compact.en-GB.Daniel".to_string(),
                name: "Daniel (UK)".to_string(),
                language: "en-GB".to_string(),
                gender: Some(yappy_core::provider::VoiceGender::Male),
                sample_url: None,
            },
        ];

        ProviderMetadata {
            id: ProviderId::new("avspeech"),
            name: "AVSpeech".to_string(),
            description: "macOS native speech synthesis (work in progress)".to_string(),
            voices,
            supported_formats: vec![AudioFormat {
                codec: AudioCodec::Pcm,
                sample_rate: SAMPLE_RATE,
                channels: 1,
                bits_per_sample: Some(16),
            }],
            options_schema: None,
        }
    }

    async fn health_check(&self) -> ProviderStatus {
        info!("AVSpeech provider health check");
        // AVSpeech is always available on macOS, but this implementation is WIP
        ProviderStatus::NotConfigured {
            reason: "AVSpeech provider is a work-in-progress. Synthesis not yet implemented."
                .to_string(),
        }
    }

    async fn synthesize(
        &self,
        _text: &str,
        _voice: &VoiceConfig,
        _format: AudioFormat,
        _cancel: CancellationToken,
    ) -> Result<AudioStream, ProviderError> {
        // This is a stub implementation
        // Full implementation requires testing objc2-avf-audio APIs on macOS
        Err(ProviderError::NotConfigured {
            reason: "AVSpeech synthesis not yet implemented. This provider requires macOS testing."
                .to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = AvSpeechConfig::default();
        assert_eq!(config.default_voice, DEFAULT_VOICE_ID);
    }

    #[test]
    fn test_provider_new() {
        let config = AvSpeechConfig {
            default_voice: "custom.voice".to_string(),
        };
        let provider = AvSpeechProvider::new(config);
        assert_eq!(provider.config().default_voice, "custom.voice");
    }

    #[test]
    fn test_provider_default() {
        let provider = AvSpeechProvider::default();
        assert_eq!(provider.config().default_voice, DEFAULT_VOICE_ID);
    }

    #[test]
    fn test_metadata() {
        let provider = AvSpeechProvider::default();
        let metadata = provider.metadata();

        assert_eq!(metadata.id.0, "avspeech");
        assert_eq!(metadata.name, "AVSpeech");
        assert!(!metadata.voices.is_empty());
        assert_eq!(metadata.supported_formats.len(), 1);
        assert_eq!(metadata.supported_formats[0].codec, AudioCodec::Pcm);
    }

    #[tokio::test]
    async fn test_health_check() {
        let provider = AvSpeechProvider::default();
        let status = provider.health_check().await;

        // WIP implementation returns NotConfigured
        assert!(matches!(status, ProviderStatus::NotConfigured { .. }));
    }

    #[tokio::test]
    async fn test_synthesize_not_implemented() {
        let provider = AvSpeechProvider::default();
        let voice = VoiceConfig::default();
        let format = AudioFormat::default();
        let cancel = CancellationToken::new();

        let result = provider.synthesize("Hello", &voice, format, cancel).await;

        // WIP implementation returns error
        assert!(matches!(result, Err(ProviderError::NotConfigured { .. })));
    }
}
