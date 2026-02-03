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

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument, trace, warn};

use yappy_core::audio::{AudioChunk, AudioCodec, AudioFormat, AudioStream};
use yappy_core::error::ProviderError;
use yappy_core::provider::{
    ProviderId, ProviderMetadata, ProviderStatus, TtsProvider, VoiceGender, VoiceInfo,
};
use yappy_core::session::VoiceConfig;

/// Default voice ID for Kokoro model
const DEFAULT_VOICE_ID: &str = "af_bella";

/// `HuggingFace` repository containing the Kokoro ONNX model
const MODEL_REPO: &str = "onnx-community/Kokoro-82M-v1.0-ONNX";

/// ONNX model filename within the repository
const MODEL_FILENAME: &str = "kokoro-v1.0.onnx";

/// Voice embeddings filename within the repository
const VOICES_FILENAME: &str = "voices-v1.0.bin";

/// Kokoro model output sample rate in Hz
const SAMPLE_RATE: u32 = 24000;

/// Audio chunk duration in milliseconds (20ms is typical for low-latency streaming)
const CHUNK_DURATION_MS: u32 = 20;

/// Samples per chunk at 24kHz: 24000 * 0.020 = 480
const SAMPLES_PER_CHUNK: usize = (SAMPLE_RATE as usize * CHUNK_DURATION_MS as usize) / 1000;

/// Maximum input token length for Kokoro model
const MAX_TOKEN_LENGTH: usize = 512;

/// Style embedding dimension (Kokoro uses 256-dimensional embeddings)
const STYLE_DIM: usize = 256;

// ============================================================================
// Phoneme Tokenization (Placeholder Implementation)
// ============================================================================

/// Placeholder phoneme tokenizer.
///
/// TODO: Replace with proper phoneme tokenization using a library like `misaki`
/// or implement IPA-based tokenization. The Kokoro model expects phoneme tokens,
/// not raw text characters.
///
/// For now, this provides a simple character-to-token mapping that will produce
/// audio output, but the quality will be poor since characters don't map directly
/// to the phoneme vocabulary the model was trained on.
struct PhonemeTokenizer {
    /// Character to token ID mapping
    char_to_token: HashMap<char, i64>,
    /// Padding token ID
    pad_token: i64,
}

impl PhonemeTokenizer {
    /// Create a new placeholder tokenizer.
    ///
    /// This builds a simple mapping from ASCII characters to token IDs.
    /// The actual Kokoro vocabulary is phoneme-based (IPA symbols), so this
    /// is just a placeholder that allows testing the synthesis pipeline.
    fn new() -> Self {
        let mut char_to_token = HashMap::new();

        // Token 0 is typically padding
        let pad_token = 0i64;

        // Build a simple vocabulary: map common characters to token IDs
        // This is NOT the actual Kokoro vocabulary, just a placeholder
        // that produces valid input tensors.
        //
        // TODO: Load the actual vocabulary from the model or implement
        // proper grapheme-to-phoneme conversion using:
        // - espeak-ng for G2P
        // - A trained G2P model
        // - The misaki library (Python, would need Rust port)

        // Space and punctuation
        char_to_token.insert(' ', 1);
        char_to_token.insert('.', 2);
        char_to_token.insert(',', 3);
        char_to_token.insert('!', 4);
        char_to_token.insert('?', 5);
        char_to_token.insert('\'', 6);
        char_to_token.insert('-', 7);
        char_to_token.insert(':', 8);
        char_to_token.insert(';', 9);

        // Lowercase letters (most common in normalized text)
        #[allow(clippy::cast_possible_wrap)]
        for (i, c) in ('a'..='z').enumerate() {
            char_to_token.insert(c, (10 + i) as i64);
        }

        // Uppercase letters (map to same tokens as lowercase for simplicity)
        #[allow(clippy::cast_possible_wrap)]
        for (i, c) in ('A'..='Z').enumerate() {
            char_to_token.insert(c, (10 + i) as i64);
        }

        // Digits
        #[allow(clippy::cast_possible_wrap)]
        for (i, c) in ('0'..='9').enumerate() {
            char_to_token.insert(c, (36 + i) as i64);
        }

        Self {
            char_to_token,
            pad_token,
        }
    }

    /// Tokenize text into a sequence of token IDs.
    ///
    /// Returns a vector of token IDs padded/truncated to fit within `MAX_TOKEN_LENGTH`.
    ///
    /// # Arguments
    /// * `text` - The input text to tokenize
    ///
    /// # Returns
    /// A vector of i64 token IDs with length <= `MAX_TOKEN_LENGTH`
    fn tokenize(&self, text: &str) -> Vec<i64> {
        let mut tokens: Vec<i64> = text
            .chars()
            .filter_map(|c| {
                self.char_to_token.get(&c).copied().or_else(|| {
                    // Unknown character - map to space token
                    trace!(char = ?c, "Unknown character, mapping to space");
                    Some(self.char_to_token[&' '])
                })
            })
            .take(MAX_TOKEN_LENGTH)
            .collect();

        // Ensure minimum length (model may require at least 1 token)
        if tokens.is_empty() {
            tokens.push(self.pad_token);
        }

        tokens
    }

    /// Get the padding token ID.
    #[allow(dead_code)]
    const fn pad_token(&self) -> i64 {
        self.pad_token
    }
}

impl Default for PhonemeTokenizer {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Voice Embedding Loading
// ============================================================================

/// Voice embeddings container.
///
/// The voices-v1.0.bin file contains pre-computed style embeddings for each voice.
/// Each voice has embeddings for different input lengths, stored as float32 arrays.
struct VoiceEmbeddings {
    /// Map from voice ID to embedding data.
    /// Each embedding is shape (`max_len`, 1, 256) stored as flat `Vec<f32>`
    embeddings: HashMap<String, Vec<f32>>,
    /// Maximum token length the embeddings support
    max_length: usize,
}

impl VoiceEmbeddings {
    /// Load voice embeddings from the binary file.
    ///
    /// The voices-v1.0.bin file format (based on Kokoro model):
    /// - Contains embeddings for multiple voices
    /// - Each voice has shape (`max_len`, 1, 256) of float32 values
    ///
    /// # Arguments
    /// * `path` - Path to the voices-v1.0.bin file
    ///
    /// # Returns
    /// `VoiceEmbeddings` or an error if loading fails
    fn load(path: &Path) -> Result<Self, ProviderError> {
        info!(?path, "Loading voice embeddings");

        let file = File::open(path).map_err(|e| ProviderError::InitializationFailed {
            message: format!("Failed to open voices file {}: {e}", path.display()),
        })?;

        let metadata = file
            .metadata()
            .map_err(|e| ProviderError::InitializationFailed {
                message: format!("Failed to get voices file metadata: {e}"),
            })?;

        #[allow(clippy::cast_possible_truncation)]
        let file_size = metadata.len() as usize;
        debug!(file_size, "Voice embeddings file size");

        let mut reader = BufReader::new(file);
        let mut buffer = vec![0u8; file_size];
        reader
            .read_exact(&mut buffer)
            .map_err(|e| ProviderError::InitializationFailed {
                message: format!("Failed to read voices file: {e}"),
            })?;

        // Parse the binary file
        // The format appears to be a simple concatenation of voice embeddings.
        // Each voice has (512, 1, 256) float32 values = 512 * 256 * 4 = 524288 bytes
        //
        // Known voices in order (based on Kokoro model):
        // af_bella, af_nicole, af_sarah, af_sky, am_adam, am_michael,
        // bf_emma, bf_isabella, bm_george, bm_lewis
        let voice_ids = [
            "af_bella",
            "af_nicole",
            "af_sarah",
            "af_sky",
            "am_adam",
            "am_michael",
            "bf_emma",
            "bf_isabella",
            "bm_george",
            "bm_lewis",
        ];

        let max_length = MAX_TOKEN_LENGTH; // 512
        let embedding_dim = STYLE_DIM; // 256
        let embedding_size = max_length * embedding_dim; // floats per voice
        let bytes_per_voice = embedding_size * 4; // 4 bytes per float32

        // Validate file size
        let expected_size = voice_ids.len() * bytes_per_voice;
        if file_size != expected_size {
            warn!(
                file_size,
                expected_size, "Voice embeddings file size mismatch, attempting to parse anyway"
            );
        }

        let mut embeddings = HashMap::new();

        for (i, voice_id) in voice_ids.iter().enumerate() {
            let start = i * bytes_per_voice;
            let end = start + bytes_per_voice;

            if end > buffer.len() {
                warn!(voice_id, "Voice embedding data truncated, skipping");
                continue;
            }

            // Convert bytes to f32
            let voice_bytes = &buffer[start..end];
            let floats: Vec<f32> = voice_bytes
                .chunks_exact(4)
                .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                .collect();

            debug!(
                voice_id,
                num_floats = floats.len(),
                "Loaded voice embedding"
            );
            embeddings.insert((*voice_id).to_string(), floats);
        }

        info!(num_voices = embeddings.len(), "Voice embeddings loaded");

        Ok(Self {
            embeddings,
            max_length,
        })
    }

    /// Get the style embedding for a voice at a specific token length.
    ///
    /// Returns a slice of the embedding of shape (1, 1, 256) for the given token count.
    ///
    /// # Arguments
    /// * `voice_id` - The voice identifier
    /// * `token_length` - Number of input tokens (determines which embedding row to use)
    ///
    /// # Returns
    /// A Vec<f32> of length 256 representing the style embedding, or an error
    fn get_embedding(
        &self,
        voice_id: &str,
        token_length: usize,
    ) -> Result<Vec<f32>, ProviderError> {
        let embedding_data =
            self.embeddings
                .get(voice_id)
                .ok_or_else(|| ProviderError::InvalidVoice {
                    voice_id: voice_id.to_string(),
                    available: self.embeddings.keys().cloned().collect(),
                })?;

        // Clamp token length to valid range
        let idx = token_length.min(self.max_length - 1);

        // Extract the embedding row for this token length
        // Shape is (max_len, 256), so row idx starts at idx * 256
        let start = idx * STYLE_DIM;
        let end = start + STYLE_DIM;

        if end > embedding_data.len() {
            return Err(ProviderError::SynthesisFailed {
                message: format!(
                    "Embedding index out of bounds for voice {voice_id}: {end} > {}",
                    embedding_data.len()
                ),
            });
        }

        Ok(embedding_data[start..end].to_vec())
    }
}

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
/// # Initialization
///
/// The provider uses lazy initialization. Call [`load_model`](Self::load_model) before
/// first use to download the model (if needed) and load the ONNX session.
///
/// ```ignore
/// let provider = KokoroProvider::with_defaults();
/// provider.load_model().await?;
/// assert!(provider.is_model_loaded());
/// ```
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

    /// Loaded voice embeddings.
    ///
    /// Populated after `load_model()` completes successfully.
    voice_embeddings: RwLock<Option<VoiceEmbeddings>>,

    /// Phoneme tokenizer (placeholder implementation).
    tokenizer: PhonemeTokenizer,
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
            voice_embeddings: RwLock::new(None),
            tokenizer: PhonemeTokenizer::new(),
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

    /// Ensure the model is loaded, loading it if necessary.
    ///
    /// This is a convenience method that:
    /// - Returns `Ok(())` immediately if the model is already loaded
    /// - Attempts to load the model if not loaded
    /// - Returns an error if loading fails
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::InitializationFailed`] if the model fails to load.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let provider = KokoroProvider::with_defaults();
    /// provider.ensure_model_loaded().await?;
    /// // Now safe to call synthesize
    /// ```
    pub async fn ensure_model_loaded(&self) -> Result<(), ProviderError> {
        if self.is_model_loaded() {
            return Ok(());
        }
        self.load_model().await
    }

    /// Load the Kokoro ONNX model.
    ///
    /// This method will:
    /// 1. Download the model from `HuggingFace` if not present locally
    /// 2. Download voice embeddings if not present
    /// 3. Initialize the ONNX runtime session
    ///
    /// The download is cached by hf-hub, so subsequent calls will use cached files.
    /// The model file is approximately 170 MB and voices file is approximately 27 MB.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::InitializationFailed`] if:
    /// - Model download fails (network error, repository not found)
    /// - ONNX session loading fails (invalid model, runtime error)
    /// - File system errors occur
    ///
    /// # Example
    ///
    /// ```ignore
    /// let provider = KokoroProvider::with_defaults();
    /// provider.load_model().await?;
    /// assert!(provider.is_model_loaded());
    /// ```
    #[instrument(skip(self), name = "kokoro_load_model")]
    pub async fn load_model(&self) -> Result<(), ProviderError> {
        // Check if already loaded
        if self.model_loaded.load(Ordering::SeqCst) {
            debug!("Kokoro model already loaded");
            return Ok(());
        }

        info!("Loading Kokoro ONNX model");

        // Determine model path: use configured path or download from HuggingFace
        let model_path = if let Some(path) = &self.config.model_path {
            debug!(?path, "Using configured model path");
            path.clone()
        } else {
            debug!(repo = MODEL_REPO, "Downloading model from HuggingFace");
            Self::ensure_model_downloaded().await?
        };

        // Determine voices path: use configured path or download from HuggingFace
        let voices_path = if let Some(path) = &self.config.voices_path {
            debug!(?path, "Using configured voices path");
            path.clone()
        } else {
            debug!(
                repo = MODEL_REPO,
                "Downloading voice embeddings from HuggingFace"
            );
            Self::ensure_voices_downloaded().await?
        };

        // Load ONNX session (blocking operation, run in spawn_blocking)
        let session = {
            let path = model_path.clone();
            tokio::task::spawn_blocking(move || Self::load_session(&path))
                .await
                .map_err(|e| ProviderError::InitializationFailed {
                    message: format!("Task join error while loading ONNX session: {e}"),
                })??
        };

        // Store the session
        {
            let mut session_guard =
                self.session
                    .write()
                    .map_err(|e| ProviderError::InitializationFailed {
                        message: format!("Failed to acquire session write lock: {e}"),
                    })?;
            *session_guard = Some(session);
        }

        // Load voice embeddings (blocking operation, run in spawn_blocking)
        let embeddings = {
            let path = voices_path;
            tokio::task::spawn_blocking(move || VoiceEmbeddings::load(&path))
                .await
                .map_err(|e| ProviderError::InitializationFailed {
                    message: format!("Task join error while loading voice embeddings: {e}"),
                })??
        };

        // Store the embeddings
        {
            let mut embeddings_guard =
                self.voice_embeddings
                    .write()
                    .map_err(|e| ProviderError::InitializationFailed {
                        message: format!("Failed to acquire embeddings write lock: {e}"),
                    })?;
            *embeddings_guard = Some(embeddings);
        }

        // Mark as loaded
        self.model_loaded.store(true, Ordering::SeqCst);

        info!("Kokoro model loaded successfully");
        Ok(())
    }

    /// Download the ONNX model from `HuggingFace` Hub.
    ///
    /// Uses hf-hub's caching mechanism, so repeated calls will use the cached file.
    /// The model file is approximately 170 MB (FP16).
    #[instrument]
    async fn ensure_model_downloaded() -> Result<PathBuf, ProviderError> {
        info!(
            repo = MODEL_REPO,
            file = MODEL_FILENAME,
            "Downloading Kokoro ONNX model (this may take a while on first run)"
        );

        let api =
            hf_hub::api::tokio::Api::new().map_err(|e| ProviderError::InitializationFailed {
                message: format!("Failed to create HuggingFace API client: {e}"),
            })?;

        let repo = api.model(MODEL_REPO.to_string());
        let model_path =
            repo.get(MODEL_FILENAME)
                .await
                .map_err(|e| ProviderError::InitializationFailed {
                    message: format!(
                        "Failed to download model from {MODEL_REPO}/{MODEL_FILENAME}: {e}"
                    ),
                })?;

        info!(?model_path, "Model downloaded successfully");
        Ok(model_path)
    }

    /// Download the voice embeddings from `HuggingFace` Hub.
    ///
    /// Uses hf-hub's caching mechanism, so repeated calls will use the cached file.
    /// The voices file is approximately 27 MB.
    #[instrument]
    async fn ensure_voices_downloaded() -> Result<PathBuf, ProviderError> {
        info!(
            repo = MODEL_REPO,
            file = VOICES_FILENAME,
            "Downloading voice embeddings"
        );

        let api =
            hf_hub::api::tokio::Api::new().map_err(|e| ProviderError::InitializationFailed {
                message: format!("Failed to create HuggingFace API client: {e}"),
            })?;

        let repo = api.model(MODEL_REPO.to_string());
        let voices_path =
            repo.get(VOICES_FILENAME)
                .await
                .map_err(|e| ProviderError::InitializationFailed {
                    message: format!(
                    "Failed to download voice embeddings from {MODEL_REPO}/{VOICES_FILENAME}: {e}"
                ),
                })?;

        info!(?voices_path, "Voice embeddings downloaded successfully");
        Ok(voices_path)
    }

    /// Load an ONNX session from the given model file.
    ///
    /// This is a blocking operation and should be called from `spawn_blocking`.
    #[instrument]
    fn load_session(model_path: &Path) -> Result<ort::session::Session, ProviderError> {
        info!(?model_path, "Loading ONNX session");

        let session = ort::session::Session::builder()
            .map_err(|e| ProviderError::InitializationFailed {
                message: format!("Failed to create ONNX session builder: {e}"),
            })?
            .commit_from_file(model_path)
            .map_err(|e| ProviderError::InitializationFailed {
                message: format!(
                    "Failed to load ONNX model from {}: {e}",
                    model_path.display()
                ),
            })?;

        info!("ONNX session loaded successfully");
        Ok(session)
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
    #[allow(clippy::significant_drop_tightening)]
    #[instrument(
        skip(self, text, cancel),
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
        cancel: CancellationToken,
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

        // Check for early cancellation
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }

        info!(text_len = text.len(), voice_id = %voice.id, "Starting synthesis");

        // 1. Tokenize text
        let tokens = self.tokenizer.tokenize(text);
        let token_len = tokens.len();
        debug!(token_len, "Text tokenized");

        // 2. Get voice embedding for this token length
        let style_embedding = {
            let embeddings_guard =
                self.voice_embeddings
                    .read()
                    .map_err(|e| ProviderError::SynthesisFailed {
                        message: format!("Failed to acquire embeddings read lock: {e}"),
                    })?;
            let embeddings =
                embeddings_guard
                    .as_ref()
                    .ok_or_else(|| ProviderError::SynthesisFailed {
                        message: "Voice embeddings not loaded".to_string(),
                    })?;
            embeddings.get_embedding(&voice.id, token_len)?
        };
        debug!(
            embedding_len = style_embedding.len(),
            "Voice embedding retrieved"
        );

        // 3. Run ONNX inference
        let audio_samples = {
            let speed = voice.speed;

            // Get mutable session reference for ONNX runtime
            let mut session_guard =
                self.session
                    .write()
                    .map_err(|e| ProviderError::SynthesisFailed {
                        message: format!("Failed to acquire session write lock: {e}"),
                    })?;
            let session = session_guard
                .as_mut()
                .ok_or_else(|| ProviderError::SynthesisFailed {
                    message: "ONNX session not loaded".to_string(),
                })?;

            // Run inference (this is blocking, but we can't use spawn_blocking
            // easily with the session reference, so we do it inline for now)
            // TODO: Consider restructuring to allow spawn_blocking for better async behavior
            Self::run_inference(session, &tokens, &style_embedding, speed)?
        };

        let num_samples = audio_samples.len();
        debug!(num_samples, "ONNX inference completed");

        // Check for cancellation after inference
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }

        // 4. Apply volume adjustment
        let audio_samples: Vec<f32> = if (voice.volume - 1.0).abs() > f32::EPSILON {
            audio_samples.iter().map(|s| s * voice.volume).collect()
        } else {
            audio_samples
        };

        // 5. Convert to requested format and create audio chunks
        let chunks = match format.codec {
            AudioCodec::Pcm => Self::create_pcm_chunks(&audio_samples),
            AudioCodec::Opus => Self::create_opus_chunks(&audio_samples)?,
            AudioCodec::Mp3 => {
                return Err(ProviderError::UnsupportedFormat {
                    format: format.codec.to_string(),
                });
            }
        };

        info!(
            num_chunks = chunks.len(),
            "Synthesis complete, returning audio stream"
        );

        // Return as a stream
        Ok(Box::pin(stream::iter(chunks.into_iter().map(Ok))))
    }
}

// ============================================================================
// Helper Functions for Synthesis
// ============================================================================

impl KokoroProvider {
    /// Run ONNX inference to generate audio samples.
    ///
    /// # Arguments
    /// * `session` - The ONNX session (mutable reference required by ort)
    /// * `tokens` - Input token IDs
    /// * `style` - Style/voice embedding (256 floats)
    /// * `speed` - Speech rate multiplier
    ///
    /// # Returns
    /// Raw audio samples as float32 at 24kHz
    fn run_inference(
        session: &mut ort::session::Session,
        tokens: &[i64],
        style: &[f32],
        speed: f32,
    ) -> Result<Vec<f32>, ProviderError> {
        use ort::value::TensorRef;

        // Create input tensors
        // input_ids: shape (1, seq_len)
        let seq_len = tokens.len();
        let input_ids_array: Vec<i64> = tokens.to_vec();

        // style: shape (1, 256)
        let style_array: Vec<f32> = style.to_vec();

        // speed: shape (1,)
        let speed_array: Vec<f32> = vec![speed];

        // Create tensor references
        let input_ids_tensor =
            TensorRef::from_array_view(([1, seq_len], input_ids_array.as_slice())).map_err(
                |e| ProviderError::SynthesisFailed {
                    message: format!("Failed to create input_ids tensor: {e}"),
                },
            )?;

        let style_tensor = TensorRef::from_array_view(([1, STYLE_DIM], style_array.as_slice()))
            .map_err(|e| ProviderError::SynthesisFailed {
                message: format!("Failed to create style tensor: {e}"),
            })?;

        let speed_tensor =
            TensorRef::from_array_view(([1usize], speed_array.as_slice())).map_err(|e| {
                ProviderError::SynthesisFailed {
                    message: format!("Failed to create speed tensor: {e}"),
                }
            })?;

        // Run inference
        // Kokoro model inputs: input_ids, style, speed
        let outputs = session
            .run(ort::inputs![
                "input_ids" => input_ids_tensor,
                "style" => style_tensor,
                "speed" => speed_tensor,
            ])
            .map_err(|e| ProviderError::SynthesisFailed {
                message: format!("ONNX inference failed: {e}"),
            })?;

        // Extract audio output
        // Output is typically the first output, shape (1, audio_length)
        let output = &outputs[0];

        // Try to extract as f32 tensor
        // try_extract_tensor returns (shape, data) tuple
        let (_shape, audio_data) =
            output
                .try_extract_tensor::<f32>()
                .map_err(|e| ProviderError::SynthesisFailed {
                    message: format!("Failed to extract audio tensor: {e}"),
                })?;

        // Convert to Vec<f32>
        let audio_samples: Vec<f32> = audio_data.to_vec();

        Ok(audio_samples)
    }

    /// Create PCM audio chunks from float32 samples.
    ///
    /// Converts float32 samples (range -1.0 to 1.0) to 16-bit PCM
    /// and splits into chunks for streaming.
    fn create_pcm_chunks(samples: &[f32]) -> Vec<AudioChunk> {
        let mut chunks = Vec::new();

        // Convert f32 samples to i16 PCM
        #[allow(clippy::cast_possible_truncation)]
        let pcm_samples: Vec<i16> = samples
            .iter()
            .map(|&s| {
                // Clamp to [-1.0, 1.0] range and convert to i16
                let clamped = s.clamp(-1.0, 1.0);
                (clamped * 32767.0) as i16
            })
            .collect();

        // Split into chunks of SAMPLES_PER_CHUNK
        for (sequence, chunk_samples) in pcm_samples.chunks(SAMPLES_PER_CHUNK).enumerate() {
            // Convert i16 samples to bytes (little-endian)
            let mut bytes = Vec::with_capacity(chunk_samples.len() * 2);
            for &sample in chunk_samples {
                bytes.extend_from_slice(&sample.to_le_bytes());
            }

            // Calculate duration for this chunk
            #[allow(clippy::cast_possible_truncation)]
            let duration_ms = (chunk_samples.len() as u32 * 1000) / SAMPLE_RATE;

            #[allow(clippy::cast_possible_truncation)]
            let seq = sequence as u32;
            chunks.push(AudioChunk::new(
                seq,
                0, // sentence_index - caller should set this
                Bytes::from(bytes),
                duration_ms,
            ));
        }

        debug!(num_chunks = chunks.len(), "Created PCM chunks");
        chunks
    }

    /// Create Opus-encoded audio chunks from float32 samples.
    ///
    /// Encodes audio using Opus codec for efficient streaming.
    fn create_opus_chunks(samples: &[f32]) -> Result<Vec<AudioChunk>, ProviderError> {
        use audiopus::{coder::Encoder, Application, Channels, SampleRate};

        // Create Opus encoder
        let encoder = Encoder::new(
            SampleRate::Hz24000,
            Channels::Mono,
            Application::Voip, // Good for speech
        )
        .map_err(|e| ProviderError::SynthesisFailed {
            message: format!("Failed to create Opus encoder: {e}"),
        })?;

        // Convert f32 samples to i16 for Opus encoder
        #[allow(clippy::cast_possible_truncation)]
        let pcm_samples: Vec<i16> = samples
            .iter()
            .map(|&s| {
                let clamped = s.clamp(-1.0, 1.0);
                (clamped * 32767.0) as i16
            })
            .collect();

        let mut chunks = Vec::new();

        // Opus frame size: 20ms at 24kHz = 480 samples
        let frame_size = SAMPLES_PER_CHUNK;

        // Encode in frames
        for (sequence, chunk_samples) in pcm_samples.chunks(frame_size).enumerate() {
            // Pad if necessary (last chunk might be smaller)
            let mut frame = chunk_samples.to_vec();
            if frame.len() < frame_size {
                frame.resize(frame_size, 0);
            }

            // Encode to Opus
            // Max Opus packet size is around 4000 bytes, but typical speech is much smaller
            let mut output = vec![0u8; 4000];
            let encoded_len = encoder.encode(&frame, &mut output).map_err(|e| {
                ProviderError::SynthesisFailed {
                    message: format!("Opus encoding failed: {e}"),
                }
            })?;

            output.truncate(encoded_len);

            // Calculate duration for this chunk
            let actual_samples = chunk_samples.len().min(frame_size);
            #[allow(clippy::cast_possible_truncation)]
            let duration_ms = (actual_samples as u32 * 1000) / SAMPLE_RATE;

            #[allow(clippy::cast_possible_truncation)]
            let seq = sequence as u32;
            chunks.push(AudioChunk::new(
                seq,
                0, // sentence_index - caller should set this
                Bytes::from(output),
                duration_ms,
            ));
        }

        debug!(num_chunks = chunks.len(), "Created Opus chunks");
        Ok(chunks)
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
    async fn test_load_model_with_invalid_path() {
        let config = KokoroConfig {
            model_path: Some(PathBuf::from("/nonexistent/path/model.onnx")),
            voices_path: Some(PathBuf::from("/nonexistent/path/voices.bin")),
            default_voice: DEFAULT_VOICE_ID.to_string(),
        };
        let provider = KokoroProvider::new(config);
        let result = provider.load_model().await;

        // Should return InitializationFailed for invalid path
        match result {
            Err(ProviderError::InitializationFailed { message }) => {
                assert!(
                    message.contains("Failed to load ONNX model"),
                    "Error should mention ONNX model loading failure: {}",
                    message
                );
            }
            Ok(_) => panic!("Expected InitializationFailed error, got Ok"),
            Err(e) => panic!("Expected InitializationFailed error, got: {:?}", e),
        }

        // Model should still be not loaded
        assert!(!provider.is_model_loaded());
    }

    #[tokio::test]
    async fn test_load_model_already_loaded_is_noop() {
        // This tests that calling load_model when already loaded returns Ok
        // We can't actually load the model in unit tests, so we test the logic
        // by checking that calling load_model multiple times doesn't panic
        let provider = KokoroProvider::with_defaults();

        // First call will fail (no network in unit tests), but shouldn't panic
        let _ = provider.load_model().await;

        // Model should not be loaded after failed attempt
        assert!(!provider.is_model_loaded());
    }

    /// Integration test for actual model loading from HuggingFace.
    /// This test is ignored by default as it requires network access
    /// and downloads ~200MB of model files.
    #[tokio::test]
    #[ignore = "requires network access and downloads large model files"]
    async fn test_load_model_from_huggingface() {
        let provider = KokoroProvider::with_defaults();
        let result = provider.load_model().await;

        assert!(result.is_ok(), "Model loading failed: {:?}", result);
        assert!(provider.is_model_loaded());

        // Verify health check returns Available
        let status = provider.health_check().await;
        assert!(
            matches!(status, ProviderStatus::Available),
            "Expected Available status, got: {:?}",
            status
        );
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

    #[tokio::test]
    async fn test_ensure_model_loaded_attempts_load() {
        // Test that ensure_model_loaded tries to load when not already loaded
        let provider = KokoroProvider::with_defaults();

        // This will attempt to download from HuggingFace but fail in unit tests
        // The important thing is it attempts the load
        let result = provider.ensure_model_loaded().await;

        // Should fail because we can't actually download in unit tests
        assert!(result.is_err());
        assert!(!provider.is_model_loaded());
    }

    /// Integration test for ensure_model_loaded from HuggingFace.
    /// This test is ignored by default as it requires network access.
    #[tokio::test]
    #[ignore = "requires network access and downloads large model files"]
    async fn test_ensure_model_loaded_from_huggingface() {
        let provider = KokoroProvider::with_defaults();

        // First call should load the model
        let result = provider.ensure_model_loaded().await;
        assert!(
            result.is_ok(),
            "First ensure_model_loaded failed: {:?}",
            result
        );
        assert!(provider.is_model_loaded());

        // Second call should be a no-op and return Ok immediately
        let result = provider.ensure_model_loaded().await;
        assert!(
            result.is_ok(),
            "Second ensure_model_loaded failed: {:?}",
            result
        );
    }

    // ========================================================================
    // Tokenizer Tests
    // ========================================================================

    #[test]
    fn test_tokenizer_basic() {
        let tokenizer = PhonemeTokenizer::new();
        let tokens = tokenizer.tokenize("Hello");

        // Should have 5 tokens for 5 characters
        assert_eq!(tokens.len(), 5);

        // All tokens should be non-negative (valid tokens)
        for token in &tokens {
            assert!(*token >= 0);
        }
    }

    #[test]
    fn test_tokenizer_empty_string() {
        let tokenizer = PhonemeTokenizer::new();
        let tokens = tokenizer.tokenize("");

        // Should have at least one token (padding)
        assert!(!tokens.is_empty());
        assert_eq!(tokens[0], tokenizer.pad_token());
    }

    #[test]
    fn test_tokenizer_with_punctuation() {
        let tokenizer = PhonemeTokenizer::new();
        let tokens = tokenizer.tokenize("Hello, world!");

        // Should handle punctuation
        assert_eq!(tokens.len(), 13); // "Hello, world!" is 13 characters
    }

    #[test]
    fn test_tokenizer_truncation() {
        let tokenizer = PhonemeTokenizer::new();

        // Create a very long string
        let long_text = "a".repeat(1000);
        let tokens = tokenizer.tokenize(&long_text);

        // Should be truncated to MAX_TOKEN_LENGTH
        assert!(tokens.len() <= MAX_TOKEN_LENGTH);
    }

    // ========================================================================
    // PCM Chunk Tests
    // ========================================================================

    #[test]
    fn test_create_pcm_chunks_basic() {
        // Create some test samples (sine wave)
        let samples: Vec<f32> = (0..SAMPLES_PER_CHUNK * 2)
            .map(|i| (i as f32 / 100.0).sin())
            .collect();

        let chunks = KokoroProvider::create_pcm_chunks(&samples);

        // Should create 2 chunks
        assert_eq!(chunks.len(), 2);

        // Each chunk should have correct duration
        assert_eq!(chunks[0].duration_ms, CHUNK_DURATION_MS);

        // Verify sequence numbers
        assert_eq!(chunks[0].sequence, 0);
        assert_eq!(chunks[1].sequence, 1);

        // Verify data size (16-bit samples = 2 bytes per sample)
        assert_eq!(chunks[0].data.len(), SAMPLES_PER_CHUNK * 2);
    }

    #[test]
    fn test_create_pcm_chunks_empty() {
        let samples: Vec<f32> = vec![];
        let chunks = KokoroProvider::create_pcm_chunks(&samples);

        // Should create no chunks for empty input
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_create_pcm_chunks_clipping() {
        // Test that values outside [-1.0, 1.0] are clipped
        let samples = vec![2.0f32, -2.0, 0.5, -0.5];
        let chunks = KokoroProvider::create_pcm_chunks(&samples);

        // Should still create a chunk
        assert_eq!(chunks.len(), 1);

        // Verify that extreme values are clipped to i16 max/min
        let data = &chunks[0].data;
        let first_sample = i16::from_le_bytes([data[0], data[1]]);
        let second_sample = i16::from_le_bytes([data[2], data[3]]);

        // 2.0 should be clipped to 1.0 -> 32767
        assert_eq!(first_sample, 32767);
        // -2.0 should be clipped to -1.0 -> -32767 (approximately)
        assert_eq!(second_sample, -32767);
    }

    // ========================================================================
    // Opus Chunk Tests
    // ========================================================================

    #[test]
    fn test_create_opus_chunks_basic() {
        // Create some test samples (sine wave)
        let samples: Vec<f32> = (0..SAMPLES_PER_CHUNK * 2)
            .map(|i| (i as f32 / 100.0).sin())
            .collect();

        let chunks = KokoroProvider::create_opus_chunks(&samples).unwrap();

        // Should create chunks
        assert!(!chunks.is_empty());

        // Verify sequence numbers
        assert_eq!(chunks[0].sequence, 0);

        // Opus data should be smaller than raw PCM
        // (This is a rough check - Opus is typically much more compressed)
        assert!(chunks[0].data.len() < SAMPLES_PER_CHUNK * 2);
    }

    #[test]
    fn test_create_opus_chunks_empty() {
        let samples: Vec<f32> = vec![];
        let chunks = KokoroProvider::create_opus_chunks(&samples).unwrap();

        // Should create no chunks for empty input
        assert!(chunks.is_empty());
    }

    // ========================================================================
    // Integration Test (requires model download)
    // ========================================================================

    /// Full synthesis integration test.
    /// This test is ignored by default as it requires network access
    /// and downloads ~200MB of model files.
    #[tokio::test]
    #[ignore = "requires network access and downloads large model files"]
    async fn test_full_synthesis_pcm() {
        let provider = KokoroProvider::with_defaults();
        provider.load_model().await.expect("Failed to load model");

        let voice = VoiceConfig {
            id: "af_bella".to_string(),
            speed: 1.0,
            pitch: 0.0,
            volume: 1.0,
        };

        let format = AudioFormat {
            codec: AudioCodec::Pcm,
            sample_rate: 24000,
            channels: 1,
            bits_per_sample: Some(16),
        };

        let cancel = CancellationToken::new();
        let result = provider
            .synthesize("Hello world", &voice, format, cancel)
            .await;

        assert!(result.is_ok(), "Synthesis failed: {:?}", result.err());

        // Collect all chunks from the stream
        use futures::StreamExt;
        let mut stream = result.unwrap();
        let mut chunks = Vec::new();
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.expect("Chunk error");
            chunks.push(chunk);
        }

        // Should have produced some audio
        assert!(!chunks.is_empty(), "No audio chunks produced");

        // Total duration should be reasonable (at least 100ms for "Hello world")
        let total_duration: u32 = chunks.iter().map(|c| c.duration_ms).sum();
        assert!(
            total_duration > 100,
            "Audio too short: {}ms",
            total_duration
        );
    }

    /// Full synthesis integration test with Opus encoding.
    #[tokio::test]
    #[ignore = "requires network access and downloads large model files"]
    async fn test_full_synthesis_opus() {
        let provider = KokoroProvider::with_defaults();
        provider.load_model().await.expect("Failed to load model");

        let voice = VoiceConfig {
            id: "am_adam".to_string(),
            speed: 1.0,
            pitch: 0.0,
            volume: 1.0,
        };

        let format = AudioFormat {
            codec: AudioCodec::Opus,
            sample_rate: 24000,
            channels: 1,
            bits_per_sample: None,
        };

        let cancel = CancellationToken::new();
        let result = provider
            .synthesize("Testing Opus encoding", &voice, format, cancel)
            .await;

        assert!(result.is_ok(), "Synthesis failed: {:?}", result.err());

        // Collect all chunks
        use futures::StreamExt;
        let mut stream = result.unwrap();
        let mut total_bytes = 0usize;
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.expect("Chunk error");
            total_bytes += chunk.data.len();
        }

        // Should have produced some audio data
        assert!(total_bytes > 0, "No audio data produced");
    }
}
