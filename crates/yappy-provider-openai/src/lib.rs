//! `OpenAI` TTS Provider
//!
//! Cloud-based TTS using `OpenAI`'s Text-to-Speech API.
//! Requires valid API key in configuration.
//!
//! # Features
//!
//! - 6 high-quality voices: alloy, echo, fable, onyx, nova, shimmer
//! - Two models: `tts-1` (fast) and `tts-1-hd` (high quality)
//! - Native Opus, MP3, AAC, FLAC, WAV, and PCM output
//! - Streaming audio response
//! - Rate limiting with exponential backoff
//! - Cancellation support
//!
//! # Example
//!
//! ```ignore
//! use yappy_provider_openai::{OpenAiProvider, OpenAiConfig};
//!
//! let config = OpenAiConfig::new("sk-your-api-key".to_string());
//! let provider = OpenAiProvider::new(config);
//!
//! // Check if configured
//! let status = provider.health_check().await;
//! ```

#![warn(missing_docs)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, instrument, trace, warn};

use yappy_core::audio::{AudioChunk, AudioCodec, AudioFormat, AudioStream};
use yappy_core::error::ProviderError;
use yappy_core::provider::{
    ProviderId, ProviderMetadata, ProviderStatus, TtsProvider, VoiceGender, VoiceInfo,
};
use yappy_core::session::VoiceConfig;

/// Default `OpenAI` TTS API endpoint
const DEFAULT_API_ENDPOINT: &str = "https://api.openai.com/v1/audio/speech";

/// Maximum input length for `OpenAI` TTS API (characters)
const MAX_INPUT_LENGTH: usize = 4096;

/// Default model for TTS (optimized for speed)
const DEFAULT_MODEL: &str = "tts-1";

/// Default voice
const DEFAULT_VOICE: &str = "alloy";

/// `OpenAI` TTS output sample rate (24kHz for all formats)
const SAMPLE_RATE: u32 = 24000;

/// Audio chunk duration in milliseconds for streaming
const CHUNK_DURATION_MS: u32 = 100;

/// Maximum retries for rate limiting
const MAX_RETRIES: u32 = 3;

/// Base delay for exponential backoff (milliseconds)
const BASE_BACKOFF_MS: u64 = 1000;

/// Maximum backoff delay (milliseconds)
const MAX_BACKOFF_MS: u64 = 30000;

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for the `OpenAI` TTS provider
#[derive(Debug, Clone)]
pub struct OpenAiConfig {
    /// `OpenAI` API key (required)
    ///
    /// Obtain from: <https://platform.openai.com/api-keys>
    pub api_key: String,

    /// TTS model to use
    ///
    /// Options:
    /// - `tts-1`: Optimized for real-time, lower latency
    /// - `tts-1-hd`: Higher quality, slightly higher latency
    pub model: String,

    /// API endpoint URL
    ///
    /// Defaults to `https://api.openai.com/v1/audio/speech`
    /// Can be overridden for testing or using proxy servers.
    pub endpoint: String,

    /// Request timeout in seconds
    pub timeout_secs: u64,
}

impl OpenAiConfig {
    /// Create a new `OpenAI` configuration with the given API key.
    ///
    /// Uses default model (`tts-1`) and endpoint.
    ///
    /// # Arguments
    ///
    /// * `api_key` - Your `OpenAI` API key
    ///
    /// # Example
    ///
    /// ```
    /// use yappy_provider_openai::OpenAiConfig;
    ///
    /// let config = OpenAiConfig::new("sk-your-api-key".to_string());
    /// assert_eq!(config.model, "tts-1");
    /// ```
    #[must_use]
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            model: DEFAULT_MODEL.to_string(),
            endpoint: DEFAULT_API_ENDPOINT.to_string(),
            timeout_secs: 30,
        }
    }

    /// Create configuration with a specific model.
    ///
    /// # Arguments
    ///
    /// * `api_key` - Your `OpenAI` API key
    /// * `model` - Model to use (`tts-1` or `tts-1-hd`)
    #[must_use]
    pub fn with_model(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            endpoint: DEFAULT_API_ENDPOINT.to_string(),
            timeout_secs: 30,
        }
    }

    /// Set a custom API endpoint.
    ///
    /// Useful for testing or proxy configurations.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: String) -> Self {
        self.endpoint = endpoint;
        self
    }

    /// Set the request timeout.
    #[must_use]
    pub const fn with_timeout(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = timeout_secs;
        self
    }

    /// Check if the API key appears to be valid format.
    ///
    /// Note: This only checks format, not actual validity with `OpenAI`.
    #[must_use]
    pub fn is_api_key_configured(&self) -> bool {
        !self.api_key.is_empty() && self.api_key.starts_with("sk-")
    }
}

impl Default for OpenAiConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: DEFAULT_MODEL.to_string(),
            endpoint: DEFAULT_API_ENDPOINT.to_string(),
            timeout_secs: 30,
        }
    }
}

// ============================================================================
// API Request/Response Types
// ============================================================================

/// Request body for `OpenAI` TTS API
#[derive(Debug, Serialize)]
struct TtsRequest<'a> {
    /// Model identifier
    model: &'a str,
    /// Input text to synthesize (max 4096 characters)
    input: &'a str,
    /// Voice to use
    voice: &'a str,
    /// Output audio format
    response_format: &'a str,
    /// Speech speed (0.25 to 4.0)
    speed: f32,
}

/// Error response from `OpenAI` API
#[derive(Debug, Deserialize)]
struct ApiErrorResponse {
    error: ApiError,
}

/// Error details from `OpenAI` API
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ApiError {
    message: String,
    #[serde(rename = "type")]
    error_type: String,
    #[serde(default)]
    code: Option<String>,
}

// ============================================================================
// OpenAI TTS Provider
// ============================================================================

/// `OpenAI` Text-to-Speech Provider
///
/// A cloud-based TTS provider using `OpenAI`'s TTS API.
///
/// # Features
///
/// - High-quality neural voices
/// - Multiple output formats (Opus, MP3, PCM, etc.)
/// - Rate limiting with exponential backoff
/// - Streaming audio response
///
/// # Rate Limiting (FR-033)
///
/// The provider implements exponential backoff for rate limiting:
/// - On 429 responses, waits and retries with increasing delays
/// - Maximum of 3 retries with delays: 1s, 2s, 4s (capped at 30s)
/// - Respects `Retry-After` header when provided
///
/// # Cancellation Support (FR-027)
///
/// The provider checks the cancellation token:
/// - Before making the API request
/// - During response streaming
/// - Aborts in-flight requests when cancelled
pub struct OpenAiProvider {
    /// Provider configuration
    config: OpenAiConfig,

    /// HTTP client for API requests
    client: Client,

    /// Counter for consecutive rate limit hits (for backoff calculation)
    rate_limit_count: AtomicU32,
}

impl OpenAiProvider {
    /// Create a new `OpenAI` provider with the given configuration.
    ///
    /// # Arguments
    ///
    /// * `config` - Provider configuration including API key
    ///
    /// # Example
    ///
    /// ```
    /// use yappy_provider_openai::{OpenAiProvider, OpenAiConfig};
    ///
    /// let config = OpenAiConfig::new("sk-your-api-key".to_string());
    /// let provider = OpenAiProvider::new(config);
    /// ```
    #[must_use]
    pub fn new(config: OpenAiConfig) -> Self {
        debug!(
            model = %config.model,
            endpoint = %config.endpoint,
            has_api_key = !config.api_key.is_empty(),
            "Creating OpenAiProvider"
        );

        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            config,
            client,
            rate_limit_count: AtomicU32::new(0),
        }
    }

    /// Get a reference to the provider configuration.
    #[must_use]
    pub const fn config(&self) -> &OpenAiConfig {
        &self.config
    }

    /// Get the list of available `OpenAI` TTS voices.
    ///
    /// `OpenAI` provides 6 voices:
    /// - `alloy`: Neutral, balanced
    /// - `echo`: Warm, conversational
    /// - `fable`: Expressive, storytelling
    /// - `onyx`: Deep, authoritative
    /// - `nova`: Friendly, optimistic
    /// - `shimmer`: Clear, professional
    #[must_use]
    pub fn available_voices() -> Vec<VoiceInfo> {
        vec![
            VoiceInfo {
                id: "alloy".to_string(),
                name: "Alloy".to_string(),
                language: "en-US".to_string(),
                gender: Some(VoiceGender::Neutral),
                sample_url: None,
            },
            VoiceInfo {
                id: "echo".to_string(),
                name: "Echo".to_string(),
                language: "en-US".to_string(),
                gender: Some(VoiceGender::Male),
                sample_url: None,
            },
            VoiceInfo {
                id: "fable".to_string(),
                name: "Fable".to_string(),
                language: "en-US".to_string(),
                gender: Some(VoiceGender::Neutral),
                sample_url: None,
            },
            VoiceInfo {
                id: "onyx".to_string(),
                name: "Onyx".to_string(),
                language: "en-US".to_string(),
                gender: Some(VoiceGender::Male),
                sample_url: None,
            },
            VoiceInfo {
                id: "nova".to_string(),
                name: "Nova".to_string(),
                language: "en-US".to_string(),
                gender: Some(VoiceGender::Female),
                sample_url: None,
            },
            VoiceInfo {
                id: "shimmer".to_string(),
                name: "Shimmer".to_string(),
                language: "en-US".to_string(),
                gender: Some(VoiceGender::Female),
                sample_url: None,
            },
        ]
    }

    /// Get the list of valid voice IDs.
    fn valid_voice_ids() -> Vec<String> {
        Self::available_voices().into_iter().map(|v| v.id).collect()
    }

    /// Map `AudioCodec` to `OpenAI` `response_format` string.
    const fn codec_to_format(codec: AudioCodec) -> &'static str {
        match codec {
            AudioCodec::Opus => "opus",
            AudioCodec::Mp3 => "mp3",
            AudioCodec::Pcm => "pcm",
        }
    }

    /// Calculate backoff delay for rate limiting with exponential backoff.
    ///
    /// Uses the formula: `min(base * 2^attempt, MAX_BACKOFF_MS)`
    fn calculate_backoff(attempt: u32, retry_after: Option<u32>) -> Duration {
        // If server provides Retry-After, use it (capped at max)
        if let Some(secs) = retry_after {
            return Duration::from_secs(u64::from(secs).min(MAX_BACKOFF_MS / 1000));
        }

        // Exponential backoff: base * 2^attempt
        let delay_ms = BASE_BACKOFF_MS.saturating_mul(1 << attempt.min(10));
        Duration::from_millis(delay_ms.min(MAX_BACKOFF_MS))
    }

    /// Parse the `Retry-After` header from response.
    fn parse_retry_after(response: &reqwest::Response) -> Option<u32> {
        response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok())
    }

    /// Make the TTS API request with retry logic for rate limiting.
    #[allow(clippy::too_many_lines)]
    #[instrument(skip(self, text, cancel), fields(text_len = text.len()))]
    async fn request_with_retry(
        &self,
        text: &str,
        voice: &str,
        format: &str,
        speed: f32,
        cancel: CancellationToken,
    ) -> Result<reqwest::Response, ProviderError> {
        let mut attempt = 0;

        loop {
            // Check for cancellation before each attempt
            if cancel.is_cancelled() {
                debug!("Request cancelled before attempt {}", attempt + 1);
                return Err(ProviderError::Cancelled);
            }

            let request_body = TtsRequest {
                model: &self.config.model,
                input: text,
                voice,
                response_format: format,
                speed,
            };

            debug!(
                attempt = attempt + 1,
                max_attempts = MAX_RETRIES + 1,
                "Sending TTS request"
            );

            let request = self
                .client
                .post(&self.config.endpoint)
                .header("Authorization", format!("Bearer {}", self.config.api_key))
                .header("Content-Type", "application/json")
                .json(&request_body);

            // Use select to allow cancellation during request
            let response_result: Result<reqwest::Response, reqwest::Error> = tokio::select! {
                result = request.send() => result,
                () = cancel.cancelled() => {
                    debug!("Request cancelled during HTTP call");
                    return Err(ProviderError::Cancelled);
                }
            };

            match response_result {
                Ok(response) => {
                    let status = response.status();

                    // Success - reset rate limit counter
                    if status.is_success() {
                        self.rate_limit_count.store(0, Ordering::SeqCst);
                        return Ok(response);
                    }

                    // Rate limited (429) - implement backoff
                    if status == StatusCode::TOO_MANY_REQUESTS {
                        let retry_after = Self::parse_retry_after(&response);
                        let backoff = Self::calculate_backoff(attempt, retry_after);

                        self.rate_limit_count.fetch_add(1, Ordering::SeqCst);

                        if attempt < MAX_RETRIES {
                            warn!(
                                attempt = attempt + 1,
                                backoff_ms = backoff.as_millis(),
                                retry_after = ?retry_after,
                                "Rate limited, backing off"
                            );

                            // Wait with cancellation support
                            tokio::select! {
                                () = sleep(backoff) => {},
                                () = cancel.cancelled() => {
                                    debug!("Request cancelled during backoff");
                                    return Err(ProviderError::Cancelled);
                                }
                            }

                            attempt += 1;
                            continue;
                        }

                        // Max retries exceeded
                        error!(attempts = attempt + 1, "Rate limit retries exhausted");
                        return Err(ProviderError::RateLimited {
                            retry_after_secs: retry_after.unwrap_or_else(|| {
                                u32::try_from(backoff.as_secs()).unwrap_or(u32::MAX)
                            }),
                        });
                    }

                    // Other error - parse error response
                    let error_text = response.text().await.unwrap_or_default();
                    let error_msg = if let Ok(api_error) =
                        serde_json::from_str::<ApiErrorResponse>(&error_text)
                    {
                        format!(
                            "{}: {}",
                            api_error.error.error_type, api_error.error.message
                        )
                    } else {
                        format!("HTTP {status}: {error_text}")
                    };

                    // Check for specific error types
                    if status == StatusCode::UNAUTHORIZED {
                        return Err(ProviderError::NotConfigured {
                            reason: "Invalid API key".to_string(),
                        });
                    }

                    if status == StatusCode::BAD_REQUEST {
                        // Could be invalid voice or other parameter
                        return Err(ProviderError::SynthesisFailed { message: error_msg });
                    }

                    error!(status = %status, error = %error_msg, "API request failed");
                    return Err(ProviderError::SynthesisFailed { message: error_msg });
                }
                Err(e) => {
                    // Network or timeout error
                    if e.is_timeout() {
                        return Err(ProviderError::Timeout {
                            timeout_secs: u32::try_from(self.config.timeout_secs)
                                .unwrap_or(u32::MAX),
                        });
                    }

                    error!(error = %e, "HTTP request failed");
                    return Err(ProviderError::SynthesisFailed {
                        message: format!("Network error: {e}"),
                    });
                }
            }
        }
    }
}

impl Default for OpenAiProvider {
    fn default() -> Self {
        Self::new(OpenAiConfig::default())
    }
}

#[async_trait]
impl TtsProvider for OpenAiProvider {
    /// Returns metadata about the `OpenAI` TTS provider.
    ///
    /// Includes provider identification, available voices (6), and supported audio formats.
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            id: ProviderId::new("openai"),
            name: "OpenAI TTS".to_string(),
            description: "Cloud-based TTS using OpenAI's Text-to-Speech API".to_string(),
            voices: Self::available_voices(),
            supported_formats: vec![
                // Opus is the default and recommended format
                AudioFormat {
                    codec: AudioCodec::Opus,
                    sample_rate: SAMPLE_RATE,
                    channels: 1,
                    bits_per_sample: None,
                },
                // MP3 for maximum compatibility
                AudioFormat {
                    codec: AudioCodec::Mp3,
                    sample_rate: SAMPLE_RATE,
                    channels: 1,
                    bits_per_sample: None,
                },
                // PCM for raw audio
                AudioFormat {
                    codec: AudioCodec::Pcm,
                    sample_rate: SAMPLE_RATE,
                    channels: 1,
                    bits_per_sample: Some(16),
                },
            ],
            options_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "model": {
                        "type": "string",
                        "enum": ["tts-1", "tts-1-hd"],
                        "default": "tts-1",
                        "description": "TTS model: tts-1 (fast) or tts-1-hd (high quality)"
                    }
                }
            })),
        }
    }

    /// Check if the provider is ready to accept synthesis requests.
    ///
    /// Returns `Available` if an API key is configured, `NotConfigured` otherwise.
    /// Note: This does not validate the API key with `OpenAI`.
    #[instrument(skip(self), name = "openai_health_check")]
    async fn health_check(&self) -> ProviderStatus {
        if self.config.is_api_key_configured() {
            debug!("OpenAI provider is available (API key configured)");
            ProviderStatus::Available
        } else if self.config.api_key.is_empty() {
            debug!("OpenAI API key not configured");
            ProviderStatus::NotConfigured {
                reason: "API key not configured. Set OPENAI_API_KEY or provide in config."
                    .to_string(),
            }
        } else {
            debug!("OpenAI API key has invalid format");
            ProviderStatus::NotConfigured {
                reason: "API key appears invalid (should start with 'sk-')".to_string(),
            }
        }
    }

    /// Synthesize text to an audio stream using `OpenAI` TTS API.
    ///
    /// # Arguments
    ///
    /// * `text` - The text to synthesize (max 4096 characters)
    /// * `voice` - Voice configuration (id, speed)
    /// * `format` - Desired audio output format
    /// * `cancel` - Cancellation token to stop synthesis early
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - API key is not configured ([`ProviderError::NotConfigured`])
    /// - Text exceeds 4096 characters ([`ProviderError::SynthesisFailed`])
    /// - Voice ID is invalid ([`ProviderError::InvalidVoice`])
    /// - Audio format is not supported ([`ProviderError::UnsupportedFormat`])
    /// - API returns an error ([`ProviderError::SynthesisFailed`])
    /// - Rate limited after retries ([`ProviderError::RateLimited`])
    /// - Request is cancelled ([`ProviderError::Cancelled`])
    #[instrument(
        skip(self, text, cancel),
        fields(
            text_len = text.len(),
            voice_id = %voice.id,
            format = ?format.codec,
        ),
        name = "openai_synthesize"
    )]
    async fn synthesize(
        &self,
        text: &str,
        voice: &VoiceConfig,
        format: AudioFormat,
        cancel: CancellationToken,
    ) -> Result<AudioStream, ProviderError> {
        // Check if API key is configured
        if !self.config.is_api_key_configured() {
            return Err(ProviderError::NotConfigured {
                reason: "OpenAI API key not configured".to_string(),
            });
        }

        // Validate text length
        if text.len() > MAX_INPUT_LENGTH {
            return Err(ProviderError::SynthesisFailed {
                message: format!(
                    "Text exceeds maximum length of {MAX_INPUT_LENGTH} characters (got {})",
                    text.len()
                ),
            });
        }

        // Validate voice ID
        let valid_voices = Self::valid_voice_ids();
        let voice_id = if voice.id.is_empty() {
            DEFAULT_VOICE
        } else if valid_voices.contains(&voice.id) {
            &voice.id
        } else {
            return Err(ProviderError::InvalidVoice {
                voice_id: voice.id.clone(),
                available: valid_voices,
            });
        };

        // Map audio format
        let format_str = Self::codec_to_format(format.codec);

        // Map voice speed (VoiceConfig uses 0.5-2.0, OpenAI uses 0.25-4.0)
        // We pass through directly since OpenAI's range is wider
        let speed = voice.speed.clamp(0.25, 4.0);

        // Check for early cancellation
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }

        info!(
            text_len = text.len(),
            voice_id = %voice_id,
            format = format_str,
            speed = speed,
            "Starting OpenAI TTS synthesis"
        );

        // Make the API request with retry logic
        let response = self
            .request_with_retry(text, voice_id, format_str, speed, cancel.clone())
            .await?;

        // Get the response body as a byte stream
        // Use .boxed() to make it Unpin for our stream processor
        let byte_stream = response.bytes_stream().boxed();

        // Create the audio stream
        let audio_stream = create_audio_stream(byte_stream, format.codec, cancel);

        info!("OpenAI TTS synthesis started, streaming audio");
        Ok(Box::pin(audio_stream))
    }
}

// ============================================================================
// Audio Stream Processing
// ============================================================================

/// Create an audio stream from the HTTP response byte stream.
///
/// Chunks the incoming bytes into `AudioChunk`s with appropriate metadata.
fn create_audio_stream<S>(
    byte_stream: S,
    codec: AudioCodec,
    cancel: CancellationToken,
) -> impl Stream<Item = Result<AudioChunk, ProviderError>> + Send + 'static
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + Unpin + 'static,
{
    // Use a simpler approach: map the byte stream to audio chunks
    // with state for sequence numbering

    struct StreamState<S> {
        stream: S,
        sequence: u32,
        buffer: Vec<u8>,
        cancel: CancellationToken,
        codec: AudioCodec,
        target_chunk_size: usize,
        done: bool,
    }

    // Determine chunk size based on codec
    let target_chunk_size = match codec {
        AudioCodec::Pcm => 4800, // ~100ms at 24kHz, 16-bit
        _ => 4096,               // For encoded formats
    };

    let state = StreamState {
        stream: byte_stream,
        sequence: 0,
        buffer: Vec::new(),
        cancel,
        codec,
        target_chunk_size,
        done: false,
    };

    futures::stream::unfold(state, |mut state| async move {
        if state.done || state.cancel.is_cancelled() {
            return None;
        }

        loop {
            // If we have enough data, return a chunk
            if state.buffer.len() >= state.target_chunk_size {
                let chunk_data: Vec<u8> = state.buffer.drain(..state.target_chunk_size).collect();
                let chunk = AudioChunk::new(
                    state.sequence,
                    0,
                    Bytes::from(chunk_data),
                    CHUNK_DURATION_MS,
                );
                state.sequence += 1;
                return Some((Ok(chunk), state));
            }

            // Try to get more data
            let next_result: Option<Result<Bytes, reqwest::Error>> = tokio::select! {
                result = state.stream.next() => result,
                () = state.cancel.cancelled() => {
                    trace!("Stream cancelled");
                    return None;
                }
            };

            match next_result {
                Some(Ok(bytes)) => {
                    trace!(bytes_received = bytes.len(), "Received bytes from OpenAI");
                    state.buffer.extend_from_slice(&bytes);
                    // Continue loop to check if we have enough data
                }
                Some(Err(e)) => {
                    error!(error = %e, "Error reading from OpenAI stream");
                    state.done = true;
                    return Some((
                        Err(ProviderError::SynthesisFailed {
                            message: format!("Stream error: {e}"),
                        }),
                        state,
                    ));
                }
                None => {
                    // Stream ended - flush remaining buffer
                    state.done = true;
                    if !state.buffer.is_empty() {
                        let chunk_data = std::mem::take(&mut state.buffer);
                        let duration = if state.codec == AudioCodec::Pcm {
                            #[allow(clippy::cast_possible_truncation)]
                            let samples = chunk_data.len() / 2;
                            #[allow(clippy::cast_possible_truncation)]
                            {
                                (samples as u64 * 1000 / u64::from(SAMPLE_RATE)) as u32
                            }
                        } else {
                            CHUNK_DURATION_MS
                        };
                        let chunk =
                            AudioChunk::new(state.sequence, 0, Bytes::from(chunk_data), duration);
                        return Some((Ok(chunk), state));
                    }
                    trace!("Stream completed");
                    return None;
                }
            }
        }
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // Configuration Tests
    // ========================================================================

    #[test]
    fn test_config_new() {
        let config = OpenAiConfig::new("sk-test-key".to_string());
        assert_eq!(config.api_key, "sk-test-key");
        assert_eq!(config.model, "tts-1");
        assert_eq!(config.endpoint, DEFAULT_API_ENDPOINT);
    }

    #[test]
    fn test_config_with_model() {
        let config = OpenAiConfig::with_model("sk-test".to_string(), "tts-1-hd".to_string());
        assert_eq!(config.model, "tts-1-hd");
    }

    #[test]
    fn test_config_with_endpoint() {
        let config = OpenAiConfig::new("sk-test".to_string())
            .with_endpoint("https://custom.api".to_string());
        assert_eq!(config.endpoint, "https://custom.api");
    }

    #[test]
    fn test_config_with_timeout() {
        let config = OpenAiConfig::new("sk-test".to_string()).with_timeout(60);
        assert_eq!(config.timeout_secs, 60);
    }

    #[test]
    fn test_config_is_api_key_configured() {
        // Valid key
        let config = OpenAiConfig::new("sk-test-key".to_string());
        assert!(config.is_api_key_configured());

        // Empty key
        let config = OpenAiConfig::default();
        assert!(!config.is_api_key_configured());

        // Invalid prefix
        let config = OpenAiConfig::new("invalid-key".to_string());
        assert!(!config.is_api_key_configured());
    }

    #[test]
    fn test_config_default() {
        let config = OpenAiConfig::default();
        assert!(config.api_key.is_empty());
        assert_eq!(config.model, DEFAULT_MODEL);
        assert_eq!(config.endpoint, DEFAULT_API_ENDPOINT);
        assert_eq!(config.timeout_secs, 30);
    }

    // ========================================================================
    // Provider Tests
    // ========================================================================

    #[test]
    fn test_provider_new() {
        let config = OpenAiConfig::new("sk-test".to_string());
        let provider = OpenAiProvider::new(config);
        assert_eq!(provider.config().api_key, "sk-test");
    }

    #[test]
    fn test_provider_default() {
        let provider = OpenAiProvider::default();
        assert!(provider.config().api_key.is_empty());
    }

    #[test]
    fn test_provider_metadata() {
        let provider = OpenAiProvider::default();
        let metadata = provider.metadata();

        assert_eq!(metadata.id.0, "openai");
        assert_eq!(metadata.name, "OpenAI TTS");
        assert!(!metadata.description.is_empty());

        // Check voices
        assert_eq!(metadata.voices.len(), 6);

        // Check supported formats
        assert_eq!(metadata.supported_formats.len(), 3);
        assert!(metadata
            .supported_formats
            .iter()
            .any(|f| f.codec == AudioCodec::Opus));
        assert!(metadata
            .supported_formats
            .iter()
            .any(|f| f.codec == AudioCodec::Mp3));
        assert!(metadata
            .supported_formats
            .iter()
            .any(|f| f.codec == AudioCodec::Pcm));

        // Check options schema
        assert!(metadata.options_schema.is_some());
    }

    #[test]
    fn test_available_voices() {
        let voices = OpenAiProvider::available_voices();

        assert_eq!(voices.len(), 6);

        let voice_ids: Vec<&str> = voices.iter().map(|v| v.id.as_str()).collect();
        assert!(voice_ids.contains(&"alloy"));
        assert!(voice_ids.contains(&"echo"));
        assert!(voice_ids.contains(&"fable"));
        assert!(voice_ids.contains(&"onyx"));
        assert!(voice_ids.contains(&"nova"));
        assert!(voice_ids.contains(&"shimmer"));

        // All voices should have en-US language
        for voice in &voices {
            assert_eq!(voice.language, "en-US");
        }

        // Check gender assignments
        let alloy = voices.iter().find(|v| v.id == "alloy").unwrap();
        assert_eq!(alloy.gender, Some(VoiceGender::Neutral));

        let nova = voices.iter().find(|v| v.id == "nova").unwrap();
        assert_eq!(nova.gender, Some(VoiceGender::Female));

        let onyx = voices.iter().find(|v| v.id == "onyx").unwrap();
        assert_eq!(onyx.gender, Some(VoiceGender::Male));
    }

    #[test]
    fn test_valid_voice_ids() {
        let ids = OpenAiProvider::valid_voice_ids();
        assert_eq!(ids.len(), 6);
        assert!(ids.contains(&"alloy".to_string()));
        assert!(ids.contains(&"shimmer".to_string()));
    }

    #[test]
    fn test_codec_to_format() {
        assert_eq!(OpenAiProvider::codec_to_format(AudioCodec::Opus), "opus");
        assert_eq!(OpenAiProvider::codec_to_format(AudioCodec::Mp3), "mp3");
        assert_eq!(OpenAiProvider::codec_to_format(AudioCodec::Pcm), "pcm");
    }

    // ========================================================================
    // Backoff Tests
    // ========================================================================

    #[test]
    fn test_calculate_backoff_no_retry_after() {
        // First attempt: 1s
        let backoff = OpenAiProvider::calculate_backoff(0, None);
        assert_eq!(backoff, Duration::from_millis(BASE_BACKOFF_MS));

        // Second attempt: 2s
        let backoff = OpenAiProvider::calculate_backoff(1, None);
        assert_eq!(backoff, Duration::from_millis(BASE_BACKOFF_MS * 2));

        // Third attempt: 4s
        let backoff = OpenAiProvider::calculate_backoff(2, None);
        assert_eq!(backoff, Duration::from_millis(BASE_BACKOFF_MS * 4));
    }

    #[test]
    fn test_calculate_backoff_with_retry_after() {
        // Server says wait 5 seconds
        let backoff = OpenAiProvider::calculate_backoff(0, Some(5));
        assert_eq!(backoff, Duration::from_secs(5));

        // Server says wait 60 seconds (should be capped)
        let backoff = OpenAiProvider::calculate_backoff(0, Some(60));
        assert_eq!(backoff, Duration::from_secs(MAX_BACKOFF_MS / 1000));
    }

    #[test]
    fn test_calculate_backoff_max_cap() {
        // Very high attempt number should be capped
        let backoff = OpenAiProvider::calculate_backoff(20, None);
        assert_eq!(backoff, Duration::from_millis(MAX_BACKOFF_MS));
    }

    // ========================================================================
    // Health Check Tests
    // ========================================================================

    #[tokio::test]
    async fn test_health_check_not_configured() {
        let provider = OpenAiProvider::default();
        let status = provider.health_check().await;

        match status {
            ProviderStatus::NotConfigured { reason } => {
                assert!(reason.contains("not configured"));
            }
            _ => panic!("Expected NotConfigured status"),
        }
    }

    #[tokio::test]
    async fn test_health_check_invalid_key_format() {
        let config = OpenAiConfig::new("invalid-key".to_string());
        let provider = OpenAiProvider::new(config);
        let status = provider.health_check().await;

        match status {
            ProviderStatus::NotConfigured { reason } => {
                assert!(reason.contains("invalid"));
            }
            _ => panic!("Expected NotConfigured status"),
        }
    }

    #[tokio::test]
    async fn test_health_check_available() {
        let config = OpenAiConfig::new("sk-test-key".to_string());
        let provider = OpenAiProvider::new(config);
        let status = provider.health_check().await;

        assert!(matches!(status, ProviderStatus::Available));
    }

    // ========================================================================
    // Synthesize Validation Tests
    // ========================================================================

    #[tokio::test]
    async fn test_synthesize_not_configured() {
        let provider = OpenAiProvider::default();
        let voice = VoiceConfig::default();
        let format = AudioFormat::default();
        let cancel = CancellationToken::new();

        let result = provider.synthesize("Hello", &voice, format, cancel).await;

        match result {
            Err(ProviderError::NotConfigured { .. }) => {}
            _ => panic!("Expected NotConfigured error"),
        }
    }

    #[tokio::test]
    async fn test_synthesize_text_too_long() {
        let config = OpenAiConfig::new("sk-test".to_string());
        let provider = OpenAiProvider::new(config);
        let voice = VoiceConfig {
            id: "alloy".to_string(),
            ..Default::default()
        };
        let format = AudioFormat::default();
        let cancel = CancellationToken::new();

        // Create text that exceeds max length
        let long_text = "a".repeat(MAX_INPUT_LENGTH + 1);
        let result = provider
            .synthesize(&long_text, &voice, format, cancel)
            .await;

        match result {
            Err(ProviderError::SynthesisFailed { message }) => {
                assert!(message.contains("exceeds maximum length"));
            }
            _ => panic!("Expected SynthesisFailed error for long text"),
        }
    }

    #[tokio::test]
    async fn test_synthesize_invalid_voice() {
        let config = OpenAiConfig::new("sk-test".to_string());
        let provider = OpenAiProvider::new(config);
        let voice = VoiceConfig {
            id: "invalid_voice".to_string(),
            ..Default::default()
        };
        let format = AudioFormat::default();
        let cancel = CancellationToken::new();

        let result = provider.synthesize("Hello", &voice, format, cancel).await;

        match result {
            Err(ProviderError::InvalidVoice {
                voice_id,
                available,
            }) => {
                assert_eq!(voice_id, "invalid_voice");
                assert_eq!(available.len(), 6);
            }
            _ => panic!("Expected InvalidVoice error"),
        }
    }

    #[tokio::test]
    async fn test_synthesize_empty_voice_uses_default() {
        // This test verifies that empty voice ID uses default
        // We can't actually call the API, but we can verify the logic
        let config = OpenAiConfig::new("sk-test".to_string());
        let provider = OpenAiProvider::new(config);
        let voice = VoiceConfig::default(); // Empty voice ID
        let format = AudioFormat::default();
        let cancel = CancellationToken::new();

        // This will fail at the network level, but not at voice validation
        let result = provider.synthesize("Hello", &voice, format, cancel).await;

        // Should NOT be an InvalidVoice error
        if let Err(ProviderError::InvalidVoice { .. }) = result {
            panic!("Empty voice should use default, not fail validation");
        }
        // Any other error is fine (network error expected)
    }

    #[tokio::test]
    async fn test_synthesize_cancelled_early() {
        let config = OpenAiConfig::new("sk-test".to_string());
        let provider = OpenAiProvider::new(config);
        let voice = VoiceConfig {
            id: "alloy".to_string(),
            ..Default::default()
        };
        let format = AudioFormat::default();
        let cancel = CancellationToken::new();

        // Cancel before calling synthesize
        cancel.cancel();

        let result = provider.synthesize("Hello", &voice, format, cancel).await;

        match result {
            Err(ProviderError::Cancelled) => {}
            _ => panic!("Expected Cancelled error"),
        }
    }

    // ========================================================================
    // TTS Request Serialization Tests
    // ========================================================================

    #[test]
    fn test_tts_request_serialization() {
        let request = TtsRequest {
            model: "tts-1",
            input: "Hello, world!",
            voice: "alloy",
            response_format: "opus",
            speed: 1.0,
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"model\":\"tts-1\""));
        assert!(json.contains("\"input\":\"Hello, world!\""));
        assert!(json.contains("\"voice\":\"alloy\""));
        assert!(json.contains("\"response_format\":\"opus\""));
        assert!(json.contains("\"speed\":1.0"));
    }

    // ========================================================================
    // Integration Tests (require API key)
    // ========================================================================

    /// Integration test that actually calls the `OpenAI` API.
    /// Ignored by default - run with: `cargo test --features openai-tts -- --ignored`
    #[tokio::test]
    #[ignore = "requires OPENAI_API_KEY environment variable"]
    async fn test_integration_synthesize() {
        let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY not set");
        let config = OpenAiConfig::new(api_key);
        let provider = OpenAiProvider::new(config);

        let voice = VoiceConfig {
            id: "alloy".to_string(),
            speed: 1.0,
            pitch: 0.0,
            volume: 1.0,
        };

        let format = AudioFormat {
            codec: AudioCodec::Opus,
            sample_rate: SAMPLE_RATE,
            channels: 1,
            bits_per_sample: None,
        };

        let cancel = CancellationToken::new();

        let result = provider
            .synthesize("Hello, this is a test.", &voice, format, cancel)
            .await;

        assert!(result.is_ok(), "Synthesis failed: {:?}", result.err());

        // Collect audio chunks
        let mut stream = result.unwrap();
        let mut total_bytes = 0usize;
        let mut chunk_count = 0usize;

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.expect("Chunk error");
            total_bytes += chunk.data.len();
            chunk_count += 1;
        }

        assert!(total_bytes > 0, "No audio data received");
        assert!(chunk_count > 0, "No chunks received");

        println!("Received {total_bytes} bytes in {chunk_count} chunks");
    }
}
