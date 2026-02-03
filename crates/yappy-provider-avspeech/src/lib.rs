//! macOS `AVSpeechSynthesizer` Provider
//!
//! Native macOS TTS using `AVFoundation`'s `AVSpeechSynthesizer`.
//! Only available on macOS targets.
//!
//! # Features
//!
//! - Native macOS speech synthesis using system voices
//! - Streaming audio output via buffer callbacks (macOS 10.15+)
//! - Access to all system-installed voices
//! - Configurable speech rate, pitch, and volume
//! - Cancellation support for mid-synthesis abort
//!
//! # Example
//!
//! ```ignore
//! use yappy_provider_avspeech::{AvSpeechProvider, AvSpeechConfig};
//!
//! let config = AvSpeechConfig::default();
//! let provider = AvSpeechProvider::new(config);
//!
//! // Provider is always available on macOS
//! let status = provider.health_check().await;
//! ```

// Allow unsafe code for Objective-C interop via objc2
#![allow(unsafe_code)]
#![warn(missing_docs)]
#![cfg(target_os = "macos")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use block2::RcBlock;
use bytes::Bytes;
use futures::stream;
use objc2::rc::Retained;
use objc2_avf_audio::{
    AVAudioBuffer, AVAudioPCMBuffer, AVSpeechSynthesisVoice, AVSpeechSynthesisVoiceGender,
    AVSpeechSynthesizer, AVSpeechUtterance,
};
use objc2_foundation::{NSArray, NSString};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument, trace, warn};

use yappy_core::audio::{AudioChunk, AudioCodec, AudioFormat, AudioStream};
use yappy_core::error::ProviderError;
use yappy_core::provider::{
    ProviderId, ProviderMetadata, ProviderStatus, TtsProvider, VoiceGender, VoiceInfo,
};
use yappy_core::session::VoiceConfig;

/// Default voice identifier (uses system default English voice if available)
const DEFAULT_VOICE_ID: &str = "com.apple.voice.compact.en-US.Samantha";

/// AVSpeech output sample rate (22050 Hz is typical for speech synthesis)
const SAMPLE_RATE: u32 = 22050;

/// Audio chunk duration in milliseconds for streaming
const CHUNK_DURATION_MS: u32 = 50;

/// Samples per chunk at 22050 Hz
const SAMPLES_PER_CHUNK: usize = (SAMPLE_RATE as usize * CHUNK_DURATION_MS as usize) / 1000;

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for the `AVSpeech` TTS provider
#[derive(Debug, Clone)]
pub struct AvSpeechConfig {
    /// Default voice identifier to use when none is specified.
    ///
    /// This should be a valid `AVSpeechSynthesisVoice` identifier, such as
    /// `"com.apple.voice.compact.en-US.Samantha"`.
    ///
    /// If the specified voice is not available, the system default will be used.
    pub default_voice: String,
}

impl Default for AvSpeechConfig {
    fn default() -> Self {
        Self {
            default_voice: DEFAULT_VOICE_ID.to_string(),
        }
    }
}

impl AvSpeechConfig {
    /// Create a new configuration with the specified default voice.
    ///
    /// # Arguments
    ///
    /// * `default_voice` - Voice identifier for the default voice
    ///
    /// # Example
    ///
    /// ```
    /// use yappy_provider_avspeech::AvSpeechConfig;
    ///
    /// let config = AvSpeechConfig::new("com.apple.voice.compact.en-US.Samantha");
    /// ```
    #[must_use]
    pub fn new(default_voice: impl Into<String>) -> Self {
        Self {
            default_voice: default_voice.into(),
        }
    }
}

// ============================================================================
// AVSpeech TTS Provider
// ============================================================================

/// macOS `AVSpeechSynthesizer` TTS Provider
///
/// A native macOS TTS provider using Apple's `AVSpeechSynthesizer` framework.
/// This provider offers access to all system-installed voices and provides
/// streaming audio output via buffer callbacks.
///
/// # Platform Support
///
/// This provider is only available on macOS. It requires macOS 10.15 (Catalina)
/// or later for the buffer callback API used for streaming.
///
/// # Audio Output
///
/// The provider outputs PCM audio (mono, 22050 Hz, 16-bit) which the server
/// can then transcode to Opus or MP3 as needed.
///
/// # Cancellation Support (FR-027)
///
/// The provider supports cancellation at multiple points:
/// - Before synthesis begins
/// - During buffer callback processing
/// - The synthesizer can be stopped immediately when cancelled
pub struct AvSpeechProvider {
    /// Provider configuration
    config: AvSpeechConfig,

    /// Cached list of available voices (populated on first access)
    cached_voices: Mutex<Option<Vec<VoiceInfo>>>,
}

impl AvSpeechProvider {
    /// Create a new `AVSpeech` provider with the given configuration.
    ///
    /// # Arguments
    ///
    /// * `config` - Provider configuration including default voice
    ///
    /// # Example
    ///
    /// ```
    /// use yappy_provider_avspeech::{AvSpeechProvider, AvSpeechConfig};
    ///
    /// let config = AvSpeechConfig::default();
    /// let provider = AvSpeechProvider::new(config);
    /// ```
    #[must_use]
    pub fn new(config: AvSpeechConfig) -> Self {
        debug!(
            default_voice = %config.default_voice,
            "Creating AvSpeechProvider"
        );
        Self {
            config,
            cached_voices: Mutex::new(None),
        }
    }

    /// Create an `AVSpeech` provider with default configuration.
    ///
    /// Uses the system's default English voice.
    ///
    /// # Example
    ///
    /// ```
    /// use yappy_provider_avspeech::AvSpeechProvider;
    ///
    /// let provider = AvSpeechProvider::with_defaults();
    /// ```
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(AvSpeechConfig::default())
    }

    /// Get a reference to the provider configuration.
    #[must_use]
    pub const fn config(&self) -> &AvSpeechConfig {
        &self.config
    }

    /// Get the list of available macOS system voices.
    ///
    /// This method queries `AVSpeechSynthesisVoice.speechVoices()` to get
    /// all installed voices on the system. The result is cached after the
    /// first call.
    ///
    /// # Returns
    ///
    /// A vector of [`VoiceInfo`] describing all available voices.
    pub fn available_voices(&self) -> Vec<VoiceInfo> {
        // Check cache first
        {
            let cache = self.cached_voices.lock().unwrap();
            if let Some(ref voices) = *cache {
                return voices.clone();
            }
        }

        // Query system voices
        let voices = Self::query_system_voices();

        // Update cache
        {
            let mut cache = self.cached_voices.lock().unwrap();
            *cache = Some(voices.clone());
        }

        voices
    }

    /// Query system voices from `AVSpeechSynthesisVoice`
    fn query_system_voices() -> Vec<VoiceInfo> {
        let mut voices = Vec::new();

        // Safety: AVSpeechSynthesisVoice.speechVoices() is safe to call
        // and returns an autoreleased NSArray
        unsafe {
            let speech_voices: Retained<NSArray<AVSpeechSynthesisVoice>> =
                AVSpeechSynthesisVoice::speechVoices();

            let count = speech_voices.count();
            debug!(voice_count = count, "Querying system voices");

            for i in 0..count {
                let voice = &speech_voices[i];

                // Get voice properties
                let identifier: Retained<NSString> = voice.identifier();
                let name: Retained<NSString> = voice.name();
                let language: Retained<NSString> = voice.language();
                let gender = voice.gender();

                let voice_info = VoiceInfo {
                    id: identifier.to_string(),
                    name: name.to_string(),
                    language: language.to_string(),
                    gender: Self::map_gender(gender),
                    sample_url: None,
                };

                trace!(
                    voice_id = %voice_info.id,
                    voice_name = %voice_info.name,
                    language = %voice_info.language,
                    "Found system voice"
                );

                voices.push(voice_info);
            }
        }

        info!(num_voices = voices.len(), "System voices enumerated");
        voices
    }

    /// Map `AVSpeechSynthesisVoiceGender` to our `VoiceGender`
    const fn map_gender(gender: AVSpeechSynthesisVoiceGender) -> Option<VoiceGender> {
        match gender {
            AVSpeechSynthesisVoiceGender::Male => Some(VoiceGender::Male),
            AVSpeechSynthesisVoiceGender::Female => Some(VoiceGender::Female),
            _ => Some(VoiceGender::Neutral),
        }
    }

    /// Get the list of valid voice identifiers.
    fn valid_voice_ids(&self) -> Vec<String> {
        self.available_voices().into_iter().map(|v| v.id).collect()
    }

    /// Create an `AVSpeechUtterance` with the given parameters.
    ///
    /// # Safety
    ///
    /// The caller must ensure this is called on the main thread or a thread
    /// with an active run loop for proper Objective-C memory management.
    unsafe fn create_utterance(
        text: &str,
        voice_id: &str,
        speed: f32,
        pitch: f32,
        volume: f32,
    ) -> Result<Retained<AVSpeechUtterance>, ProviderError> {
        // Create NSString from text
        let ns_text = NSString::from_str(text);

        // Create utterance
        let utterance = AVSpeechUtterance::speechUtteranceWithString(&ns_text);

        // Set voice if specified
        if !voice_id.is_empty() {
            let ns_voice_id = NSString::from_str(voice_id);
            if let Some(voice) = AVSpeechSynthesisVoice::voiceWithIdentifier(&ns_voice_id) {
                utterance.setVoice(Some(&voice));
            } else {
                warn!(voice_id = %voice_id, "Voice not found, using system default");
            }
        }

        // Map speech rate: VoiceConfig uses 0.5-2.0, AVSpeechUtterance uses 0.0-1.0
        // where 0.5 is the default rate
        // Formula: av_rate = (voice_speed - 0.5) / 3.0 + 0.5
        // This maps: 0.5 -> 0.5 (default), 2.0 -> 1.0 (max), 0.5 -> 0.5
        let av_rate = ((speed - 1.0) * 0.25 + 0.5).clamp(0.0, 1.0);
        utterance.setRate(av_rate);

        // Map pitch: VoiceConfig uses -1.0 to 1.0, AVSpeechUtterance uses 0.5-2.0
        // where 1.0 is the default
        // Formula: av_pitch = pitch + 1.0
        let av_pitch = (pitch + 1.0).clamp(0.5, 2.0);
        utterance.setPitchMultiplier(av_pitch);

        // Volume: both use 0.0-1.0 range
        utterance.setVolume(volume.clamp(0.0, 1.0));

        debug!(
            av_rate = av_rate,
            av_pitch = av_pitch,
            volume = volume,
            "Created utterance with parameters"
        );

        Ok(utterance)
    }

    /// Synthesize speech using the buffer callback API.
    ///
    /// This method uses `writeUtterance:toBufferCallback:` to get streaming
    /// audio buffers as they are generated. The audio is PCM format which
    /// we convert to our `AudioChunk` format.
    #[allow(clippy::too_many_lines)]
    async fn synthesize_with_buffer_callback(
        &self,
        text: &str,
        voice: &VoiceConfig,
        cancel: CancellationToken,
    ) -> Result<Vec<AudioChunk>, ProviderError> {
        // Determine which voice to use
        let voice_id = if voice.id.is_empty() {
            self.config.default_voice.clone()
        } else {
            voice.id.clone()
        };

        // Clone values for the blocking task
        let text = text.to_string();
        let speed = voice.speed;
        let pitch = voice.pitch;
        let volume = voice.volume;

        // Run the synthesis on a blocking thread since Objective-C calls
        // should ideally happen on the main thread or a dedicated thread
        let result = tokio::task::spawn_blocking(move || {
            // Shared state for collecting audio chunks
            let chunks: Arc<Mutex<Vec<AudioChunk>>> = Arc::new(Mutex::new(Vec::new()));
            let sequence = Arc::new(std::sync::atomic::AtomicU32::new(0));
            let cancelled = Arc::new(AtomicBool::new(false));
            let synthesis_done = Arc::new(AtomicBool::new(false));

            // Clone references for the callback
            let chunks_ref = Arc::clone(&chunks);
            let sequence_ref = Arc::clone(&sequence);
            let cancelled_ref = Arc::clone(&cancelled);
            let synthesis_done_ref = Arc::clone(&synthesis_done);
            let cancel_ref = cancel.clone();

            unsafe {
                // Create the synthesizer
                let synthesizer = AVSpeechSynthesizer::new();

                // Create the utterance
                let utterance = Self::create_utterance(&text, &voice_id, speed, pitch, volume)?;

                // Create the buffer callback block
                // The callback receives AVAudioBuffer which we process into AudioChunks
                let callback = RcBlock::new(move |buffer: &AVAudioBuffer| {
                    // Check for cancellation
                    if cancel_ref.is_cancelled() {
                        cancelled_ref.store(true, Ordering::SeqCst);
                        return;
                    }

                    // Try to cast to PCM buffer for audio data access
                    // AVAudioBuffer is the base class; we need AVAudioPCMBuffer for samples
                    let pcm_buffer: &AVAudioPCMBuffer =
                        &*(buffer as *const AVAudioBuffer as *const AVAudioPCMBuffer);

                    // Get the frame count (number of samples)
                    let frame_count = pcm_buffer.frameLength() as usize;
                    if frame_count == 0 {
                        return;
                    }

                    // Get the float channel data
                    // AVSpeech typically outputs mono float32 audio
                    let float_data = pcm_buffer.floatChannelData();
                    if float_data.is_null() {
                        trace!("Buffer has no float channel data");
                        return;
                    }

                    // Get the first channel (mono)
                    // NonNull is guaranteed non-null, so no null check needed
                    let channel_ptr: std::ptr::NonNull<f32> = *float_data;

                    // Read samples from the buffer
                    let samples = std::slice::from_raw_parts(channel_ptr.as_ptr(), frame_count);

                    // Convert float32 to 16-bit PCM and split into chunks
                    let mut current_chunk_samples = Vec::with_capacity(SAMPLES_PER_CHUNK);

                    for &sample in samples {
                        // Convert float to i16
                        #[allow(clippy::cast_possible_truncation)]
                        let pcm_sample = (sample.clamp(-1.0, 1.0) * 32767.0) as i16;
                        current_chunk_samples.push(pcm_sample);

                        // When we have enough samples for a chunk, emit it
                        if current_chunk_samples.len() >= SAMPLES_PER_CHUNK {
                            let chunk_data = samples_to_bytes(&current_chunk_samples);
                            let seq = sequence_ref.fetch_add(1, Ordering::SeqCst);

                            let chunk = AudioChunk::new(
                                seq,
                                0, // sentence_index - caller should set this
                                Bytes::from(chunk_data),
                                CHUNK_DURATION_MS,
                            );

                            let mut chunks_guard = chunks_ref.lock().unwrap();
                            chunks_guard.push(chunk);
                            current_chunk_samples.clear();
                        }
                    }

                    // Don't emit partial chunks here - we'll handle remaining samples
                    // at the end of synthesis
                    if !current_chunk_samples.is_empty() {
                        // For now, pad and emit partial chunks to avoid losing audio
                        let chunk_data = samples_to_bytes(&current_chunk_samples);
                        #[allow(clippy::cast_possible_truncation)]
                        let duration_ms = (current_chunk_samples.len() as u32 * 1000) / SAMPLE_RATE;
                        let seq = sequence_ref.fetch_add(1, Ordering::SeqCst);

                        let chunk = AudioChunk::new(seq, 0, Bytes::from(chunk_data), duration_ms);

                        let mut chunks_guard = chunks_ref.lock().unwrap();
                        chunks_guard.push(chunk);
                    }
                });

                // Start synthesis with buffer callback
                // This method writes audio to the callback instead of playing it
                debug!(text_len = text.len(), "Starting synthesis");
                synthesizer.writeUtterance_toBufferCallback(&utterance, &callback);

                // Mark synthesis as complete
                synthesis_done_ref.store(true, Ordering::SeqCst);
            }

            // Check if synthesis was cancelled
            if cancelled.load(Ordering::SeqCst) {
                return Err(ProviderError::Cancelled);
            }

            // Return the collected chunks
            let final_chunks = chunks.lock().unwrap().clone();
            Ok(final_chunks)
        })
        .await
        .map_err(|e| ProviderError::SynthesisFailed {
            message: format!("Synthesis task failed: {e}"),
        })??;

        Ok(result)
    }
}

/// Convert i16 PCM samples to bytes (little-endian)
fn samples_to_bytes(samples: &[i16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for &sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

impl Default for AvSpeechProvider {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[async_trait]
impl TtsProvider for AvSpeechProvider {
    /// Returns metadata about the `AVSpeech` provider.
    ///
    /// Includes provider identification, available system voices, and supported audio formats.
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            id: ProviderId::new("avspeech"),
            name: "AVSpeech".to_string(),
            description: "Native macOS TTS using AVSpeechSynthesizer".to_string(),
            voices: self.available_voices(),
            // AVSpeech outputs PCM; transcoding to Opus/MP3 is handled by the server
            supported_formats: vec![AudioFormat {
                codec: AudioCodec::Pcm,
                sample_rate: SAMPLE_RATE,
                channels: 1,
                bits_per_sample: Some(16),
            }],
            options_schema: None,
        }
    }

    /// Check if the provider is ready to accept synthesis requests.
    ///
    /// `AVSpeech` is always available on macOS, so this always returns `Available`.
    #[instrument(skip(self), name = "avspeech_health_check")]
    async fn health_check(&self) -> ProviderStatus {
        // AVSpeech is a system framework, always available on macOS
        debug!("AVSpeech provider is available (macOS system framework)");
        ProviderStatus::Available
    }

    /// Synthesize text to an audio stream using `AVSpeechSynthesizer`.
    ///
    /// # Arguments
    ///
    /// * `text` - The text to synthesize
    /// * `voice` - Voice configuration (id, speed, pitch, volume)
    /// * `format` - Desired audio output format (must be PCM; transcoding handled by server)
    /// * `cancel` - Cancellation token to stop synthesis early
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Voice ID is specified but not found ([`ProviderError::InvalidVoice`])
    /// - Audio format is not PCM ([`ProviderError::UnsupportedFormat`])
    /// - Synthesis fails ([`ProviderError::SynthesisFailed`])
    /// - Request is cancelled ([`ProviderError::Cancelled`])
    #[instrument(
        skip(self, text, cancel),
        fields(
            text_len = text.len(),
            voice_id = %voice.id,
            format = ?format.codec,
        ),
        name = "avspeech_synthesize"
    )]
    async fn synthesize(
        &self,
        text: &str,
        voice: &VoiceConfig,
        format: AudioFormat,
        cancel: CancellationToken,
    ) -> Result<AudioStream, ProviderError> {
        // Validate audio format - we only support PCM output
        // (server handles transcoding to Opus/MP3)
        if format.codec != AudioCodec::Pcm {
            return Err(ProviderError::UnsupportedFormat {
                format: format!("{} (AVSpeech only outputs PCM)", format.codec),
            });
        }

        // Validate voice ID if specified
        if !voice.id.is_empty() {
            let valid_ids = self.valid_voice_ids();
            if !valid_ids.contains(&voice.id) {
                return Err(ProviderError::InvalidVoice {
                    voice_id: voice.id.clone(),
                    available: valid_ids,
                });
            }
        }

        // Check for early cancellation
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }

        info!(
            text_len = text.len(),
            voice_id = %voice.id,
            "Starting AVSpeech synthesis"
        );

        // Perform synthesis with buffer callback
        let chunks = self
            .synthesize_with_buffer_callback(text, voice, cancel)
            .await?;

        info!(num_chunks = chunks.len(), "AVSpeech synthesis complete");

        // Return chunks as a stream
        Ok(Box::pin(stream::iter(chunks.into_iter().map(Ok))))
    }
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
    fn test_config_default() {
        let config = AvSpeechConfig::default();
        assert_eq!(config.default_voice, DEFAULT_VOICE_ID);
    }

    #[test]
    fn test_config_new() {
        let config = AvSpeechConfig::new("com.apple.voice.custom");
        assert_eq!(config.default_voice, "com.apple.voice.custom");
    }

    // ========================================================================
    // Provider Tests
    // ========================================================================

    #[test]
    fn test_provider_new() {
        let config = AvSpeechConfig::new("com.apple.voice.test");
        let provider = AvSpeechProvider::new(config);
        assert_eq!(provider.config().default_voice, "com.apple.voice.test");
    }

    #[test]
    fn test_provider_with_defaults() {
        let provider = AvSpeechProvider::with_defaults();
        assert_eq!(provider.config().default_voice, DEFAULT_VOICE_ID);
    }

    #[test]
    fn test_provider_default_trait() {
        let provider = AvSpeechProvider::default();
        assert_eq!(provider.config().default_voice, DEFAULT_VOICE_ID);
    }

    #[test]
    fn test_provider_metadata() {
        let provider = AvSpeechProvider::with_defaults();
        let metadata = provider.metadata();

        assert_eq!(metadata.id.0, "avspeech");
        assert_eq!(metadata.name, "AVSpeech");
        assert!(!metadata.description.is_empty());

        // Check supported formats - should only be PCM
        assert_eq!(metadata.supported_formats.len(), 1);
        assert_eq!(metadata.supported_formats[0].codec, AudioCodec::Pcm);
        assert_eq!(metadata.supported_formats[0].sample_rate, SAMPLE_RATE);
        assert_eq!(metadata.supported_formats[0].channels, 1);
        assert_eq!(metadata.supported_formats[0].bits_per_sample, Some(16));

        // No options schema for this provider
        assert!(metadata.options_schema.is_none());
    }

    // ========================================================================
    // Voice Enumeration Tests
    // ========================================================================

    #[test]
    fn test_available_voices() {
        let provider = AvSpeechProvider::with_defaults();
        let voices = provider.available_voices();

        // Should have at least some voices on any macOS system
        assert!(!voices.is_empty(), "Expected at least one system voice");

        // Each voice should have required fields
        for voice in &voices {
            assert!(!voice.id.is_empty(), "Voice ID should not be empty");
            assert!(!voice.name.is_empty(), "Voice name should not be empty");
            assert!(
                !voice.language.is_empty(),
                "Voice language should not be empty"
            );
        }
    }

    #[test]
    fn test_voice_caching() {
        let provider = AvSpeechProvider::with_defaults();

        // First call should query system voices
        let voices1 = provider.available_voices();

        // Second call should return cached result
        let voices2 = provider.available_voices();

        // Should be the same
        assert_eq!(voices1.len(), voices2.len());
        for (v1, v2) in voices1.iter().zip(voices2.iter()) {
            assert_eq!(v1.id, v2.id);
            assert_eq!(v1.name, v2.name);
        }
    }

    #[test]
    fn test_valid_voice_ids() {
        let provider = AvSpeechProvider::with_defaults();
        let ids = provider.valid_voice_ids();

        // Should have at least some voice IDs
        assert!(!ids.is_empty());

        // All IDs should be non-empty strings
        for id in &ids {
            assert!(!id.is_empty());
        }
    }

    // ========================================================================
    // Gender Mapping Tests
    // ========================================================================

    #[test]
    fn test_map_gender() {
        assert_eq!(
            AvSpeechProvider::map_gender(AVSpeechSynthesisVoiceGender::Male),
            Some(VoiceGender::Male)
        );
        assert_eq!(
            AvSpeechProvider::map_gender(AVSpeechSynthesisVoiceGender::Female),
            Some(VoiceGender::Female)
        );
        assert_eq!(
            AvSpeechProvider::map_gender(AVSpeechSynthesisVoiceGender::Unspecified),
            Some(VoiceGender::Neutral)
        );
    }

    // ========================================================================
    // Samples to Bytes Tests
    // ========================================================================

    #[test]
    fn test_samples_to_bytes() {
        let samples = vec![0i16, 32767, -32768, 1000, -1000];
        let bytes = samples_to_bytes(&samples);

        // Should have 2 bytes per sample
        assert_eq!(bytes.len(), samples.len() * 2);

        // Verify little-endian encoding
        assert_eq!(&bytes[0..2], &0i16.to_le_bytes());
        assert_eq!(&bytes[2..4], &32767i16.to_le_bytes());
        assert_eq!(&bytes[4..6], &(-32768i16).to_le_bytes());
    }

    #[test]
    fn test_samples_to_bytes_empty() {
        let samples: Vec<i16> = vec![];
        let bytes = samples_to_bytes(&samples);
        assert!(bytes.is_empty());
    }

    // ========================================================================
    // Health Check Tests
    // ========================================================================

    #[tokio::test]
    async fn test_health_check_available() {
        let provider = AvSpeechProvider::with_defaults();
        let status = provider.health_check().await;

        // AVSpeech is always available on macOS
        assert!(matches!(status, ProviderStatus::Available));
    }

    // ========================================================================
    // Synthesize Validation Tests
    // ========================================================================

    #[tokio::test]
    async fn test_synthesize_unsupported_format() {
        let provider = AvSpeechProvider::with_defaults();
        let voice = VoiceConfig::default();
        let format = AudioFormat {
            codec: AudioCodec::Opus, // Not supported - we only output PCM
            sample_rate: 48000,
            channels: 1,
            bits_per_sample: None,
        };
        let cancel = CancellationToken::new();

        let result = provider.synthesize("Hello", &voice, format, cancel).await;

        match result {
            Err(ProviderError::UnsupportedFormat { format }) => {
                assert!(format.contains("PCM"));
            }
            _ => panic!("Expected UnsupportedFormat error"),
        }
    }

    #[tokio::test]
    async fn test_synthesize_cancelled_early() {
        let provider = AvSpeechProvider::with_defaults();
        let voice = VoiceConfig::default();
        let format = AudioFormat {
            codec: AudioCodec::Pcm,
            sample_rate: SAMPLE_RATE,
            channels: 1,
            bits_per_sample: Some(16),
        };
        let cancel = CancellationToken::new();

        // Cancel before synthesis
        cancel.cancel();

        let result = provider.synthesize("Hello", &voice, format, cancel).await;

        match result {
            Err(ProviderError::Cancelled) => {}
            _ => panic!("Expected Cancelled error"),
        }
    }

    // ========================================================================
    // Integration Tests
    // ========================================================================

    /// Integration test for actual speech synthesis.
    /// This test actually synthesizes audio and verifies we get output.
    #[tokio::test]
    async fn test_synthesize_basic() {
        let provider = AvSpeechProvider::with_defaults();
        let voice = VoiceConfig::default();
        let format = AudioFormat {
            codec: AudioCodec::Pcm,
            sample_rate: SAMPLE_RATE,
            channels: 1,
            bits_per_sample: Some(16),
        };
        let cancel = CancellationToken::new();

        let result = provider
            .synthesize("Hello, world!", &voice, format, cancel)
            .await;

        assert!(result.is_ok(), "Synthesis failed: {:?}", result.err());

        // Collect chunks
        use futures::StreamExt;
        let mut stream = result.unwrap();
        let mut total_bytes = 0usize;
        let mut chunk_count = 0usize;

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.expect("Chunk error");
            total_bytes += chunk.data.len();
            chunk_count += 1;
        }

        // Should have produced some audio
        assert!(total_bytes > 0, "No audio data produced");
        assert!(chunk_count > 0, "No chunks produced");

        println!("Synthesized {total_bytes} bytes in {chunk_count} chunks");
    }

    /// Test synthesis with a specific system voice.
    #[tokio::test]
    async fn test_synthesize_with_voice() {
        let provider = AvSpeechProvider::with_defaults();
        let voices = provider.available_voices();

        // Skip if no voices available
        if voices.is_empty() {
            println!("No system voices available, skipping test");
            return;
        }

        // Use the first available voice
        let voice = VoiceConfig {
            id: voices[0].id.clone(),
            speed: 1.0,
            pitch: 0.0,
            volume: 1.0,
        };

        let format = AudioFormat {
            codec: AudioCodec::Pcm,
            sample_rate: SAMPLE_RATE,
            channels: 1,
            bits_per_sample: Some(16),
        };
        let cancel = CancellationToken::new();

        let result = provider
            .synthesize("Testing voice selection.", &voice, format, cancel)
            .await;

        assert!(result.is_ok(), "Synthesis failed: {:?}", result.err());
    }

    /// Test synthesis with custom speed.
    #[tokio::test]
    async fn test_synthesize_with_speed() {
        let provider = AvSpeechProvider::with_defaults();
        let voice = VoiceConfig {
            id: String::new(),
            speed: 1.5, // 1.5x speed
            pitch: 0.0,
            volume: 1.0,
        };

        let format = AudioFormat {
            codec: AudioCodec::Pcm,
            sample_rate: SAMPLE_RATE,
            channels: 1,
            bits_per_sample: Some(16),
        };
        let cancel = CancellationToken::new();

        let result = provider
            .synthesize("Testing speed adjustment.", &voice, format, cancel)
            .await;

        assert!(result.is_ok(), "Synthesis failed: {:?}", result.err());
    }
}
