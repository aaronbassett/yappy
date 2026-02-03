//! Mock TTS provider for integration tests
//!
//! Provides a simple mock provider that returns static audio data,
//! avoiding the need for ONNX model loading in tests.

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::stream;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use yappy_core::{
    audio::{AudioChunk, AudioFormat, AudioStream},
    error::ProviderError,
    provider::{ProviderId, ProviderMetadata, ProviderStatus, VoiceInfo},
    session::VoiceConfig,
    TtsProvider,
};

/// Configuration for mock synthesis behavior
#[derive(Clone)]
pub enum MockSynthesisMode {
    /// Return a fixed number of audio chunks per sentence
    Success {
        /// Number of chunks per sentence
        chunks_per_sentence: usize,
        /// Duration in ms for each chunk
        chunk_duration_ms: u32,
        /// Bytes of audio data per chunk
        chunk_bytes: usize,
    },
    /// Return an error during synthesis initialization
    FailInit {
        /// Error message
        error: String,
    },
    /// Return an error mid-stream after some chunks
    FailMidStream {
        /// Chunks before the error
        chunks_before_error: usize,
        /// Error message
        error: String,
    },
}

impl Default for MockSynthesisMode {
    fn default() -> Self {
        Self::Success {
            chunks_per_sentence: 1,
            chunk_duration_ms: 100,
            chunk_bytes: 256,
        }
    }
}

/// Mock TTS provider for testing
///
/// This provider generates static audio data without requiring
/// actual model inference. It can be configured to simulate
/// various success and failure scenarios.
pub struct MockTtsProvider {
    /// Provider ID
    id: String,
    /// Human-readable name
    name: String,
    /// Provider status (available/unavailable)
    status: ProviderStatus,
    /// Available voices
    voices: Vec<VoiceInfo>,
    /// Synthesis behavior configuration
    synthesis_mode: MockSynthesisMode,
    /// Count of synthesize calls (for testing)
    synthesize_count: AtomicUsize,
    /// Whether provider should be marked available
    available: AtomicBool,
}

impl MockTtsProvider {
    /// Create a new mock provider with default settings
    pub fn new(id: &str) -> Self {
        Self {
            id: id.to_string(),
            name: format!("Mock {}", id),
            status: ProviderStatus::Available,
            voices: vec![
                VoiceInfo {
                    id: "test_voice_1".to_string(),
                    name: "Test Voice 1".to_string(),
                    language: "en-US".to_string(),
                    gender: None,
                    sample_url: None,
                },
                VoiceInfo {
                    id: "test_voice_2".to_string(),
                    name: "Test Voice 2".to_string(),
                    language: "en-GB".to_string(),
                    gender: None,
                    sample_url: None,
                },
            ],
            synthesis_mode: MockSynthesisMode::default(),
            synthesize_count: AtomicUsize::new(0),
            available: AtomicBool::new(true),
        }
    }

    /// Create a mock provider with custom synthesis mode
    pub fn with_synthesis_mode(mut self, mode: MockSynthesisMode) -> Self {
        self.synthesis_mode = mode;
        self
    }

    /// Create an unavailable mock provider
    pub fn unavailable(id: &str, reason: &str) -> Self {
        Self {
            id: id.to_string(),
            name: format!("Mock {}", id),
            status: ProviderStatus::Unavailable {
                reason: reason.to_string(),
            },
            voices: vec![],
            synthesis_mode: MockSynthesisMode::default(),
            synthesize_count: AtomicUsize::new(0),
            available: AtomicBool::new(false),
        }
    }

    /// Get the number of times synthesize was called
    pub fn synthesize_count(&self) -> usize {
        self.synthesize_count.load(Ordering::SeqCst)
    }

    /// Reset the synthesize call counter
    pub fn reset_count(&self) {
        self.synthesize_count.store(0, Ordering::SeqCst);
    }
}

#[async_trait]
impl TtsProvider for MockTtsProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            id: ProviderId::new(&self.id),
            name: self.name.clone(),
            description: format!("Mock {} provider for testing", self.name),
            voices: self.voices.clone(),
            supported_formats: vec![AudioFormat::default()],
            options_schema: None,
        }
    }

    async fn health_check(&self) -> ProviderStatus {
        if self.available.load(Ordering::SeqCst) {
            ProviderStatus::Available
        } else {
            self.status.clone()
        }
    }

    async fn synthesize(
        &self,
        _text: &str,
        _voice: &VoiceConfig,
        _format: AudioFormat,
        _cancel: CancellationToken,
    ) -> Result<AudioStream, ProviderError> {
        self.synthesize_count.fetch_add(1, Ordering::SeqCst);

        match &self.synthesis_mode {
            MockSynthesisMode::Success {
                chunks_per_sentence,
                chunk_duration_ms,
                chunk_bytes,
            } => {
                let mut items: Vec<Result<AudioChunk, ProviderError>> = Vec::new();

                for i in 0..*chunks_per_sentence {
                    // Generate recognizable test audio data pattern
                    // Pattern: [0xAA, sequence_byte, ... padding]
                    let mut data = vec![0xAA; *chunk_bytes];
                    if *chunk_bytes > 0 {
                        data[0] = 0xAA; // Marker byte
                    }
                    if *chunk_bytes > 1 {
                        data[1] = i as u8; // Chunk index within sentence
                    }

                    items.push(Ok(AudioChunk::new(
                        i as u32, // Will be reassigned by caller
                        0,        // Will be reassigned by caller
                        Bytes::from(data),
                        *chunk_duration_ms,
                    )));
                }

                Ok(Box::pin(stream::iter(items)))
            }
            MockSynthesisMode::FailInit { error } => Err(ProviderError::SynthesisFailed {
                message: error.clone(),
            }),
            MockSynthesisMode::FailMidStream {
                chunks_before_error,
                error,
            } => {
                let mut items: Vec<Result<AudioChunk, ProviderError>> = Vec::new();

                // Add successful chunks
                for i in 0..*chunks_before_error {
                    items.push(Ok(AudioChunk::new(
                        i as u32,
                        0,
                        Bytes::from(vec![0xAA; 256]),
                        100,
                    )));
                }

                // Add failure
                items.push(Err(ProviderError::SynthesisFailed {
                    message: error.clone(),
                }));

                Ok(Box::pin(stream::iter(items)))
            }
        }
    }
}

/// Create a thread-safe mock provider wrapped in Arc
pub fn create_mock_provider(id: &str) -> Arc<MockTtsProvider> {
    Arc::new(MockTtsProvider::new(id))
}

/// Create a failing mock provider
pub fn create_failing_provider(id: &str, error: &str) -> MockTtsProvider {
    MockTtsProvider::new(id).with_synthesis_mode(MockSynthesisMode::FailInit {
        error: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_provider_metadata() {
        let provider = MockTtsProvider::new("test");
        let metadata = provider.metadata();

        assert_eq!(metadata.id.0, "test");
        assert_eq!(metadata.voices.len(), 2);
        assert_eq!(metadata.supported_formats.len(), 1);
    }

    #[tokio::test]
    async fn test_mock_provider_health_check() {
        let provider = MockTtsProvider::new("test");
        let status = provider.health_check().await;
        assert!(status.is_available());

        let unavailable = MockTtsProvider::unavailable("test", "not configured");
        let status = unavailable.health_check().await;
        assert!(!status.is_available());
    }

    #[tokio::test]
    async fn test_mock_provider_synthesize_success() {
        use futures_util::StreamExt;

        let provider = MockTtsProvider::new("test");
        let cancel = CancellationToken::new();

        let stream = provider
            .synthesize(
                "Test text",
                &VoiceConfig::default(),
                AudioFormat::default(),
                cancel,
            )
            .await
            .expect("synthesize should succeed");

        let chunks: Vec<_> = stream.collect().await;
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].is_ok());

        let chunk = chunks[0].as_ref().unwrap();
        assert_eq!(chunk.duration_ms, 100);
        assert_eq!(chunk.data.len(), 256);
    }

    #[tokio::test]
    async fn test_mock_provider_synthesize_count() {
        let provider = MockTtsProvider::new("test");
        let cancel = CancellationToken::new();

        assert_eq!(provider.synthesize_count(), 0);

        let _ = provider
            .synthesize(
                "Test",
                &VoiceConfig::default(),
                AudioFormat::default(),
                cancel.clone(),
            )
            .await;
        assert_eq!(provider.synthesize_count(), 1);

        let _ = provider
            .synthesize(
                "Test",
                &VoiceConfig::default(),
                AudioFormat::default(),
                cancel,
            )
            .await;
        assert_eq!(provider.synthesize_count(), 2);

        provider.reset_count();
        assert_eq!(provider.synthesize_count(), 0);
    }
}
