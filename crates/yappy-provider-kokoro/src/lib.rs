//! Kokoro ONNX TTS Provider
//!
//! Local TTS using the Kokoro 82M ONNX model.
//! Downloads model from `HuggingFace` on first use.
//!
//! # Voice Naming Convention
//!
//! Kokoro voices use the format: `{gender}{ethnicity}_{name}`
//! - `af` = American Female
//! - `am` = American Male
//! - `bf` = British Female
//! - `bm` = British Male
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
//!     voices_path: Some("/path/to/voices".into()),
//!     default_voice: "af_bella".to_string(),
//! };
//! let provider = KokoroProvider::new(config);
//! ```

#![warn(missing_docs)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument, warn};

use yappy_core::audio::{AudioCodec, AudioFormat, AudioStream};
use yappy_core::error::ProviderError;
use yappy_core::provider::{
    ProviderId, ProviderMetadata, ProviderStatus, TtsProvider, VoiceGender, VoiceInfo,
};
use yappy_core::session::VoiceConfig;

/// Default voice ID for Kokoro model
const DEFAULT_VOICE_ID: &str = "af_bella";

/// HuggingFace repository containing the Kokoro ONNX model
const MODEL_REPO: &str = "onnx-community/Kokoro-82M-v1.0-ONNX";

/// ONNX model filename within the repository
const MODEL_FILENAME: &str = "kokoro-v1.0.onnx";

/// Voice embeddings filename within the repository
const VOICES_FILENAME: &str = "voices-v1.0.bin";

/// Configuration for the Kokoro TTS provider
#[derive(Debug, Clone)]
pub struct KokoroConfig {
    /// Path to the ONNX model file.
    ///
    /// If `None`, the model will be automatically downloaded from `HuggingFace`
    /// Hub on first use and cached locally.
    pub model_path: Option<PathBuf>,

    /// Path to the voice embeddings directory.
    ///
    /// If `None`, voice embeddings will be downloaded from `HuggingFace`
    /// Hub alongside the model.
    pub voices_path: Option<PathBuf>,

    /// Default voice ID to use when none is specified.
    ///
    /// Kokoro supports multiple voice styles. See [`KokoroProvider::available_voices`]
    /// for available options.
    pub default_voice: String,
}

impl Default for KokoroConfig {
    fn default() -> Self {
        Self {
            model_path: None,
            voices_path: None,
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
/// - 10 voice styles across American and British English
/// - Supports Opus and PCM audio output formats
/// - Optimized for real-time streaming synthesis
///
/// # Model State
///
/// The provider can be in one of two states:
/// - **Uninitialized**: Model not loaded, calls to `synthesize` will fail
/// - **Loaded**: Model loaded and ready for synthesis
///
/// Call [`load_model`](Self::load_model) to transition from uninitialized to loaded.
///
/// # Status
///
/// This is a skeleton implementation. The ONNX session loading and
/// audio synthesis will be implemented in T076-T078.
pub struct KokoroProvider {
    /// Provider configuration
    config: KokoroConfig,

    /// Whether the model has been loaded and is ready for synthesis
    model_loaded: AtomicBool,

    /// ONNX Runtime session for model inference.
    ///
    /// Wrapped in `RwLock<Option<...>>` for thread-safe lazy initialization.
    /// The session is loaded when `load_model()` is called.
    session: RwLock<Option<ort::session::Session>>,

    /// Path to the downloaded voice embeddings file.
    ///
    /// Populated after `load_model()` completes successfully.
    voices_file_path: RwLock<Option<PathBuf>>,
}

impl KokoroProvider {
    /// Create a new Kokoro provider with the given configuration.
    ///
    /// Note: This does not load the ONNX model. Call [`load_model`](Self::load_model)
    /// to download (if necessary) and initialize the model.
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
    ///     voices_path: Some("/models/voices".into()),
    ///     default_voice: "af_bella".to_string(),
    /// };
    /// let provider = KokoroProvider::new(config);
    /// ```
    pub fn new(config: KokoroConfig) -> Self {
        debug!(
            model_path = ?config.model_path,
            voices_path = ?config.voices_path,
            default_voice = %config.default_voice,
            "Creating KokoroProvider"
        );
        Self {
            config,
            model_loaded: AtomicBool::new(false),
            session: RwLock::new(None),
            voices_file_path: RwLock::new(None),
        }
    }

    /// Create a Kokoro provider with default configuration.
    ///
    /// Uses the default voice (`af_bella`) and will download the model
    /// from `HuggingFace` Hub when [`load_model`](Self::load_model) is called.
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

    /// Check if the model is currently loaded and ready for synthesis.
    #[must_use]
    pub fn is_model_loaded(&self) -> bool {
        self.model_loaded.load(Ordering::SeqCst)
    }

    /// Load the Kokoro ONNX model.
    ///
    /// This method will:
    /// 1. Download the model from `HuggingFace` if not present locally
    /// 2. Download voice embeddings if not present
    /// 3. Initialize the ONNX runtime session
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Model download fails
    /// - Model file is corrupted or invalid
    /// - ONNX runtime initialization fails
    ///
    /// # Note
    ///
    /// This is a stub implementation. Actual model loading will be
    /// implemented in T076-T078.
    #[instrument(skip(self), name = "kokoro_load_model")]
    pub async fn load_model(&mut self) -> Result<(), ProviderError> {
        // TODO: Implement actual model loading (T076)
        // 1. Use hf-hub to download model if not present
        // 2. Load voice embeddings
        // 3. Initialize ort::Session

        warn!("Kokoro model loading not yet implemented");

        // For now, just mark as not loaded
        self.model_loaded.store(false, Ordering::SeqCst);

        Err(ProviderError::NotConfigured {
            reason: "Kokoro model loading not yet implemented (coming in T076-T078)".to_string(),
        })
    }

    /// Get the list of available Kokoro voices.
    ///
    /// # Voice Naming Convention
    ///
    /// Voice IDs follow the pattern `{gender}{ethnicity}_{name}`:
    /// - `af` = American Female
    /// - `am` = American Male
    /// - `bf` = British Female
    /// - `bm` = British Male
    ///
    /// # Returns
    ///
    /// A vector of [`VoiceInfo`] describing all 10 available voices.
    #[must_use]
    pub fn available_voices() -> Vec<VoiceInfo> {
        vec![
            // American Female voices
            VoiceInfo {
                id: "af_bella".to_string(),
                name: "Bella (American Female)".to_string(),
                language: "en-US".to_string(),
                gender: Some(VoiceGender::Female),
                sample_url: None,
            },
            VoiceInfo {
                id: "af_nicole".to_string(),
                name: "Nicole (American Female)".to_string(),
                language: "en-US".to_string(),
                gender: Some(VoiceGender::Female),
                sample_url: None,
            },
            VoiceInfo {
                id: "af_sarah".to_string(),
                name: "Sarah (American Female)".to_string(),
                language: "en-US".to_string(),
                gender: Some(VoiceGender::Female),
                sample_url: None,
            },
            VoiceInfo {
                id: "af_sky".to_string(),
                name: "Sky (American Female)".to_string(),
                language: "en-US".to_string(),
                gender: Some(VoiceGender::Female),
                sample_url: None,
            },
            // American Male voices
            VoiceInfo {
                id: "am_adam".to_string(),
                name: "Adam (American Male)".to_string(),
                language: "en-US".to_string(),
                gender: Some(VoiceGender::Male),
                sample_url: None,
            },
            VoiceInfo {
                id: "am_michael".to_string(),
                name: "Michael (American Male)".to_string(),
                language: "en-US".to_string(),
                gender: Some(VoiceGender::Male),
                sample_url: None,
            },
            // British Female voices
            VoiceInfo {
                id: "bf_emma".to_string(),
                name: "Emma (British Female)".to_string(),
                language: "en-GB".to_string(),
                gender: Some(VoiceGender::Female),
                sample_url: None,
            },
            VoiceInfo {
                id: "bf_isabella".to_string(),
                name: "Isabella (British Female)".to_string(),
                language: "en-GB".to_string(),
                gender: Some(VoiceGender::Female),
                sample_url: None,
            },
            // British Male voices
            VoiceInfo {
                id: "bm_george".to_string(),
                name: "George (British Male)".to_string(),
                language: "en-GB".to_string(),
                gender: Some(VoiceGender::Male),
                sample_url: None,
            },
            VoiceInfo {
                id: "bm_lewis".to_string(),
                name: "Lewis (British Male)".to_string(),
                language: "en-GB".to_string(),
                gender: Some(VoiceGender::Male),
                sample_url: None,
            },
        ]
    }

    /// Get the list of valid voice IDs.
    fn valid_voice_ids() -> Vec<String> {
        Self::available_voices().into_iter().map(|v| v.id).collect()
    }
}

impl Default for KokoroProvider {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[async_trait]
impl TtsProvider for KokoroProvider {
    /// Returns metadata about the Kokoro provider.
    ///
    /// Includes provider identification, available voices, and supported audio formats.
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            id: ProviderId::new("kokoro"),
            name: "Kokoro".to_string(),
            description: "Local TTS using Kokoro 82M ONNX model".to_string(),
            voices: Self::available_voices(),
            supported_formats: vec![
                // Opus is the preferred format (good compression, low latency)
                AudioFormat {
                    codec: AudioCodec::Opus,
                    sample_rate: 24000,
                    channels: 1,
                    bits_per_sample: None,
                },
                // PCM for maximum compatibility
                AudioFormat {
                    codec: AudioCodec::Pcm,
                    sample_rate: 24000,
                    channels: 1,
                    bits_per_sample: Some(16),
                },
            ],
            options_schema: None,
        }
    }

    /// Check if the provider is ready to accept synthesis requests.
    ///
    /// # Returns
    ///
    /// - [`ProviderStatus::Available`] if model is loaded and ready
    /// - [`ProviderStatus::NotConfigured`] if model is not loaded, with a reason
    #[instrument(skip(self), name = "kokoro_health_check")]
    async fn health_check(&self) -> ProviderStatus {
        if self.model_loaded.load(Ordering::SeqCst) {
            info!("Kokoro provider is available");
            ProviderStatus::Available
        } else if self.config.model_path.is_none() {
            debug!("Kokoro model path not configured");
            ProviderStatus::NotConfigured {
                reason: "Model not loaded. Call load_model() to download and initialize."
                    .to_string(),
            }
        } else {
            debug!(
                model_path = ?self.config.model_path,
                "Kokoro model path set but not loaded"
            );
            ProviderStatus::NotConfigured {
                reason:
                    "Model path configured but model not loaded. Call load_model() to initialize."
                        .to_string(),
            }
        }
    }

    /// Synthesize text to an audio stream.
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
    /// Returns an error if:
    /// - Model is not loaded ([`ProviderError::NotConfigured`])
    /// - Voice ID is invalid ([`ProviderError::InvalidVoice`])
    /// - Audio format is not supported ([`ProviderError::UnsupportedFormat`])
    /// - Synthesis fails ([`ProviderError::SynthesisFailed`])
    ///
    /// # Note
    ///
    /// This is a stub implementation. Actual synthesis will be implemented
    /// in T076-T078.
    #[instrument(
        skip(self, text, _cancel),
        fields(
            text_len = text.len(),
            voice_id = %voice.id,
            format = ?format.codec,
        ),
        name = "kokoro_synthesize"
    )]
    async fn synthesize(
        &self,
        text: &str,
        voice: &VoiceConfig,
        format: AudioFormat,
        _cancel: CancellationToken,
    ) -> Result<AudioStream, ProviderError> {
        // Check if model is loaded
        if !self.model_loaded.load(Ordering::SeqCst) {
            return Err(ProviderError::NotConfigured {
                reason: "Kokoro model not loaded. Call load_model() first.".to_string(),
            });
        }

        // Validate voice ID
        let valid_voice_ids = Self::valid_voice_ids();
        if !valid_voice_ids.contains(&voice.id) {
            return Err(ProviderError::InvalidVoice {
                voice_id: voice.id.clone(),
                available: valid_voice_ids,
            });
        }

        // Validate audio format
        let supported_codecs = [AudioCodec::Opus, AudioCodec::Pcm];
        if !supported_codecs.contains(&format.codec) {
            return Err(ProviderError::UnsupportedFormat {
                format: format.codec.to_string(),
            });
        }

        // TODO: Implement actual synthesis in T076-T078
        // 1. Tokenize text
        // 2. Run ONNX inference
        // 3. Encode audio to requested format
        // 4. Return as AudioStream

        warn!(
            text_len = text.len(),
            voice = %voice.id,
            "Kokoro synthesis not yet implemented"
        );

        Err(ProviderError::SynthesisFailed {
            message: "Kokoro synthesis not yet implemented. Coming in T076-T078.".to_string(),
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
        assert!(config.voices_path.is_none());
        assert_eq!(config.default_voice, "af_bella");
    }

    #[test]
    fn test_with_defaults() {
        let provider = KokoroProvider::with_defaults();
        assert!(provider.config().model_path.is_none());
        assert!(provider.config().voices_path.is_none());
        assert_eq!(provider.config().default_voice, "af_bella");
        assert!(!provider.is_model_loaded());
    }

    #[test]
    fn test_custom_config() {
        let config = KokoroConfig {
            model_path: Some(PathBuf::from("/custom/path/model.onnx")),
            voices_path: Some(PathBuf::from("/custom/path/voices")),
            default_voice: "am_adam".to_string(),
        };
        let provider = KokoroProvider::new(config);

        assert_eq!(
            provider.config().model_path,
            Some(PathBuf::from("/custom/path/model.onnx"))
        );
        assert_eq!(
            provider.config().voices_path,
            Some(PathBuf::from("/custom/path/voices"))
        );
        assert_eq!(provider.config().default_voice, "am_adam");
    }

    #[test]
    fn test_metadata() {
        let provider = KokoroProvider::with_defaults();
        let metadata = provider.metadata();

        assert_eq!(metadata.id.0, "kokoro");
        assert_eq!(metadata.name, "Kokoro");
        assert_eq!(
            metadata.description,
            "Local TTS using Kokoro 82M ONNX model"
        );

        // Check voices
        assert_eq!(metadata.voices.len(), 10);

        // Check that we have both Opus and PCM formats
        assert_eq!(metadata.supported_formats.len(), 2);
        assert!(metadata
            .supported_formats
            .iter()
            .any(|f| f.codec == AudioCodec::Opus));
        assert!(metadata
            .supported_formats
            .iter()
            .any(|f| f.codec == AudioCodec::Pcm));
    }

    #[test]
    fn test_available_voices() {
        let voices = KokoroProvider::available_voices();

        // Verify we have 10 voices
        assert_eq!(voices.len(), 10);

        // Verify expected voice IDs
        let voice_ids: Vec<&str> = voices.iter().map(|v| v.id.as_str()).collect();
        assert!(voice_ids.contains(&"af_bella"));
        assert!(voice_ids.contains(&"af_nicole"));
        assert!(voice_ids.contains(&"af_sarah"));
        assert!(voice_ids.contains(&"af_sky"));
        assert!(voice_ids.contains(&"am_adam"));
        assert!(voice_ids.contains(&"am_michael"));
        assert!(voice_ids.contains(&"bf_emma"));
        assert!(voice_ids.contains(&"bf_isabella"));
        assert!(voice_ids.contains(&"bm_george"));
        assert!(voice_ids.contains(&"bm_lewis"));

        // Verify American voices have en-US language
        for voice in voices.iter().filter(|v| v.id.starts_with("a")) {
            assert_eq!(
                voice.language, "en-US",
                "American voice {} should have en-US language",
                voice.id
            );
        }

        // Verify British voices have en-GB language
        for voice in voices.iter().filter(|v| v.id.starts_with("b")) {
            assert_eq!(
                voice.language, "en-GB",
                "British voice {} should have en-GB language",
                voice.id
            );
        }

        // Verify gender assignments for female voices
        for voice in voices.iter().filter(|v| v.id.contains("f_")) {
            assert_eq!(
                voice.gender,
                Some(VoiceGender::Female),
                "Voice {} should be female",
                voice.id
            );
        }

        // Verify gender assignments for male voices
        for voice in voices.iter().filter(|v| v.id.contains("m_")) {
            assert_eq!(
                voice.gender,
                Some(VoiceGender::Male),
                "Voice {} should be male",
                voice.id
            );
        }
    }

    #[tokio::test]
    async fn test_health_check_not_configured() {
        let provider = KokoroProvider::with_defaults();
        let status = provider.health_check().await;

        match status {
            ProviderStatus::NotConfigured { reason } => {
                assert!(
                    reason.contains("not loaded"),
                    "Reason should mention model not loaded: {}",
                    reason
                );
            }
            _ => panic!("Expected NotConfigured status"),
        }
    }

    #[tokio::test]
    async fn test_health_check_with_path_but_not_loaded() {
        let config = KokoroConfig {
            model_path: Some(PathBuf::from("/some/path/model.onnx")),
            voices_path: None,
            default_voice: DEFAULT_VOICE_ID.to_string(),
        };
        let provider = KokoroProvider::new(config);
        let status = provider.health_check().await;

        match status {
            ProviderStatus::NotConfigured { reason } => {
                assert!(
                    reason.contains("not loaded"),
                    "Reason should mention model not loaded: {}",
                    reason
                );
            }
            _ => panic!("Expected NotConfigured status"),
        }
    }

    #[tokio::test]
    async fn test_synthesize_not_loaded() {
        let provider = KokoroProvider::with_defaults();
        let voice = VoiceConfig {
            id: "af_bella".to_string(),
            speed: 1.0,
            pitch: 0.0,
            volume: 1.0,
        };
        let format = AudioFormat::default();
        let cancel = CancellationToken::new();

        let result = provider.synthesize("Hello", &voice, format, cancel).await;

        match result {
            Err(ProviderError::NotConfigured { reason }) => {
                assert!(
                    reason.contains("not loaded"),
                    "Error should mention model not loaded: {}",
                    reason
                );
            }
            Ok(_) => panic!("Expected NotConfigured error, got Ok"),
            Err(e) => panic!("Expected NotConfigured error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_load_model_stub() {
        let mut provider = KokoroProvider::with_defaults();
        let result = provider.load_model().await;

        // Should return NotConfigured since it's a stub
        match result {
            Err(ProviderError::NotConfigured { reason }) => {
                assert!(
                    reason.contains("not yet implemented"),
                    "Error should mention not implemented: {}",
                    reason
                );
            }
            _ => panic!("Expected NotConfigured error, got: {:?}", result),
        }

        // Model should still be not loaded
        assert!(!provider.is_model_loaded());
    }

    #[test]
    fn test_valid_voice_ids() {
        let voice_ids = KokoroProvider::valid_voice_ids();
        assert_eq!(voice_ids.len(), 10);
        assert!(voice_ids.contains(&"af_bella".to_string()));
        assert!(voice_ids.contains(&"bm_lewis".to_string()));
    }

    #[test]
    fn test_default_trait() {
        let provider = KokoroProvider::default();
        assert_eq!(provider.config().default_voice, "af_bella");
    }
}
