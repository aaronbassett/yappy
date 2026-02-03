//! WebSocket integration tests for basic TTS flow
//!
//! These tests verify the WebSocket protocol implementation using a mock TTS
//! provider to avoid model dependencies. The tests exercise the full WebSocket
//! connection lifecycle from HTTP upgrade through message exchange.
//!
//! # Test Coverage
//!
//! - Session initialization (`session.init` -> `session.ready`)
//! - Text message handling and sentence extraction
//! - Binary audio frame format (12-byte header + data)
//! - Session completion (`text.done` -> `audio.done`)
//! - Error handling for invalid messages and protocol violations
//!
//! # Running Tests
//!
//! ```bash
//! cargo test -p yappy-server --test ws_basic
//! ```

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::stream;
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use yappy_core::{
    audio::{AudioChunk, AudioFormat, AudioStream},
    config::{BufferConfigToml, Config, ProvidersConfig, ServerConfig},
    error::ProviderError,
    message::{ClientMessage, ServerMessage},
    provider::{ProviderId, ProviderMetadata, ProviderStatus, VoiceInfo},
    session::VoiceConfig,
    AudioCodec, TtsProvider,
};
use yappy_server::{create_router, AppState, ProviderRegistry};

// ============================================================================
// Mock TTS Provider
// ============================================================================

/// Mock TTS provider for testing.
///
/// Returns predictable audio data so tests can verify the full flow without
/// requiring real TTS models or API keys.
struct MockTtsProvider {
    /// Provider ID
    id: String,
    /// Provider name
    name: String,
    /// Provider health status
    status: ProviderStatus,
    /// Available voices
    voices: Vec<VoiceInfo>,
    /// Behavior mode for synthesis
    synthesis_mode: MockSynthesisMode,
}

/// Configurable synthesis behavior for testing different scenarios.
#[derive(Clone)]
#[allow(dead_code)] // Variants reserved for future test scenarios
enum MockSynthesisMode {
    /// Return predictable audio chunks
    Success {
        /// Number of chunks per sentence
        chunks_per_sentence: usize,
        /// Duration in ms for each chunk
        chunk_duration_ms: u32,
        /// Bytes of audio data per chunk
        chunk_bytes: usize,
    },
    /// Fail on synthesis init (reserved for error handling tests)
    FailInit { error: String },
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

impl MockTtsProvider {
    /// Create a new mock provider with default settings.
    fn new(id: &str, name: &str) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            status: ProviderStatus::Available,
            voices: vec![VoiceInfo {
                id: "test_voice".to_string(),
                name: "Test Voice".to_string(),
                language: "en-US".to_string(),
                gender: None,
                sample_url: None,
            }],
            synthesis_mode: MockSynthesisMode::default(),
        }
    }

    /// Create a mock provider with custom synthesis mode.
    fn with_synthesis_mode(mut self, mode: MockSynthesisMode) -> Self {
        self.synthesis_mode = mode;
        self
    }

    /// Create an unavailable mock provider (reserved for provider health tests).
    #[allow(dead_code)]
    fn unavailable(id: &str, name: &str, reason: &str) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            status: ProviderStatus::Unavailable {
                reason: reason.to_string(),
            },
            voices: vec![],
            synthesis_mode: MockSynthesisMode::default(),
        }
    }
}

#[async_trait]
impl TtsProvider for MockTtsProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            id: ProviderId::new(&self.id),
            name: self.name.clone(),
            description: format!("Mock {} provider for testing", &self.name),
            voices: self.voices.clone(),
            supported_formats: vec![AudioFormat::default()],
            options_schema: None,
        }
    }

    async fn health_check(&self) -> ProviderStatus {
        self.status.clone()
    }

    async fn synthesize(
        &self,
        _text: &str,
        _voice: &VoiceConfig,
        _format: AudioFormat,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<AudioStream, ProviderError> {
        match &self.synthesis_mode {
            MockSynthesisMode::Success {
                chunks_per_sentence,
                chunk_duration_ms,
                chunk_bytes,
            } => {
                // Generate predictable audio chunks
                #[allow(clippy::cast_possible_truncation)]
                let chunks: Vec<Result<AudioChunk, ProviderError>> = (0..*chunks_per_sentence)
                    .map(|i| {
                        Ok(AudioChunk::new(
                            i as u32,
                            0, // sentence_index will be set by caller
                            // Use recognizable pattern: 0xAB repeated
                            Bytes::from(vec![0xAB; *chunk_bytes]),
                            *chunk_duration_ms,
                        ))
                    })
                    .collect();
                Ok(Box::pin(stream::iter(chunks)))
            }
            MockSynthesisMode::FailInit { error } => Err(ProviderError::SynthesisFailed {
                message: error.clone(),
            }),
        }
    }
}

// ============================================================================
// Test Server Setup
// ============================================================================

/// Start a test server with the given provider registry.
///
/// Returns the server address and a handle that can be used to shut down
/// the server (by dropping it).
async fn start_test_server(
    registry: ProviderRegistry,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let config = Config {
        server: ServerConfig::default(),
        providers: ProvidersConfig {
            default: "mock".to_string(),
            openai: None,
            kokoro: None,
            avspeech: None,
        },
        buffer: BufferConfigToml::default(),
    };

    let state = AppState::new(config, registry);
    let router = create_router(state);

    // Bind to port 0 to get a random available port
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    // Small delay to ensure server is ready
    tokio::time::sleep(Duration::from_millis(10)).await;

    (addr, handle)
}

/// Create a provider registry with a working mock provider.
fn create_mock_registry() -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    registry.register(MockTtsProvider::new("mock", "Mock TTS Provider"));
    registry.set_default(ProviderId::new("mock"));
    registry
}

/// Create a provider registry with a mock that generates multiple chunks.
fn create_multi_chunk_registry() -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    registry.register(
        MockTtsProvider::new("mock", "Mock TTS Provider").with_synthesis_mode(
            MockSynthesisMode::Success {
                chunks_per_sentence: 3,
                chunk_duration_ms: 50,
                chunk_bytes: 128,
            },
        ),
    );
    registry.set_default(ProviderId::new("mock"));
    registry
}

/// Create an empty provider registry.
fn create_empty_registry() -> ProviderRegistry {
    ProviderRegistry::new()
}

/// Helper to connect to WebSocket endpoint.
async fn connect_ws(
    addr: SocketAddr,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let url = format!("ws://{addr}/ws");
    let (ws, _) = connect_async(&url)
        .await
        .expect("Failed to connect to WebSocket");
    ws
}

/// Default timeout for test operations.
const TEST_TIMEOUT: Duration = Duration::from_secs(5);

// ============================================================================
// Session Initialization Tests
// ============================================================================

/// Test basic session initialization flow.
///
/// Verifies that:
/// 1. Client can connect via WebSocket
/// 2. Sending `session.init` returns `session.ready`
/// 3. Response contains valid session ID, audio format, and voice
#[tokio::test]
async fn test_websocket_session_init() {
    let registry = create_mock_registry();
    let (addr, _handle) = start_test_server(registry).await;

    let mut ws = connect_ws(addr).await;

    // Send session.init
    let init_msg = ClientMessage::SessionInit {
        provider: None,
        voice: None,
        audio_format: None,
        code_block_mode: None,
    };
    ws.send(Message::Text(
        serde_json::to_string(&init_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    // Receive session.ready
    let response = timeout(TEST_TIMEOUT, ws.next())
        .await
        .expect("Timeout waiting for response")
        .expect("Stream closed")
        .expect("WebSocket error");

    if let Message::Text(text) = response {
        let server_msg: ServerMessage = serde_json::from_str(&text).unwrap();
        match server_msg {
            ServerMessage::SessionReady {
                session_id,
                audio_format,
                voice,
            } => {
                // Session ID should have expected prefix
                assert!(
                    session_id.starts_with("ses_"),
                    "Session ID should start with 'ses_': {session_id}",
                );
                // Voice should be the default from mock provider
                assert_eq!(voice, "test_voice");
                // Audio format should be Opus (default)
                assert_eq!(audio_format.codec, AudioCodec::Opus);
            }
            _ => panic!("Expected SessionReady, got: {server_msg:?}"),
        }
    } else {
        panic!("Expected text message, got: {response:?}");
    }

    // Clean close
    ws.close(None).await.ok();
}

/// Test session initialization with explicit provider.
#[tokio::test]
async fn test_websocket_session_init_with_provider() {
    let registry = create_mock_registry();
    let (addr, _handle) = start_test_server(registry).await;

    let mut ws = connect_ws(addr).await;

    // Send session.init with explicit provider
    let init_msg = ClientMessage::SessionInit {
        provider: Some("mock".to_string()),
        voice: None,
        audio_format: None,
        code_block_mode: None,
    };
    ws.send(Message::Text(
        serde_json::to_string(&init_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    let response = timeout(TEST_TIMEOUT, ws.next())
        .await
        .expect("Timeout")
        .expect("Stream closed")
        .expect("WebSocket error");

    if let Message::Text(text) = response {
        let server_msg: ServerMessage = serde_json::from_str(&text).unwrap();
        assert!(
            matches!(server_msg, ServerMessage::SessionReady { .. }),
            "Expected SessionReady"
        );
    } else {
        panic!("Expected text message");
    }

    ws.close(None).await.ok();
}

/// Test session initialization with custom voice parameters.
///
/// Uses the provider's existing voice with custom speed parameter.
#[tokio::test]
async fn test_websocket_session_init_with_custom_voice() {
    let registry = create_mock_registry();
    let (addr, _handle) = start_test_server(registry).await;

    let mut ws = connect_ws(addr).await;

    // Send session.init with the provider's voice and custom speed
    // The mock provider has "test_voice" as its only voice
    let init_msg = ClientMessage::SessionInit {
        provider: None,
        voice: Some(VoiceConfig {
            id: "test_voice".to_string(),
            speed: 1.5,
            pitch: 0.0,
            volume: 1.0,
        }),
        audio_format: None,
        code_block_mode: None,
    };
    ws.send(Message::Text(
        serde_json::to_string(&init_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    let response = timeout(TEST_TIMEOUT, ws.next())
        .await
        .expect("Timeout")
        .expect("Stream closed")
        .expect("WebSocket error");

    if let Message::Text(text) = response {
        let server_msg: ServerMessage = serde_json::from_str(&text).unwrap();
        match server_msg {
            ServerMessage::SessionReady { voice, .. } => {
                assert_eq!(voice, "test_voice");
            }
            _ => panic!("Expected SessionReady, got: {server_msg:?}"),
        }
    } else {
        panic!("Expected text message");
    }

    ws.close(None).await.ok();
}

/// Test session initialization with invalid voice returns error.
#[tokio::test]
async fn test_websocket_session_init_with_invalid_voice() {
    let registry = create_mock_registry();
    let (addr, _handle) = start_test_server(registry).await;

    let mut ws = connect_ws(addr).await;

    // Send session.init with a voice that doesn't exist in the provider
    let init_msg = ClientMessage::SessionInit {
        provider: None,
        voice: Some(VoiceConfig {
            id: "nonexistent_voice".to_string(),
            speed: 1.0,
            pitch: 0.0,
            volume: 1.0,
        }),
        audio_format: None,
        code_block_mode: None,
    };
    ws.send(Message::Text(
        serde_json::to_string(&init_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    let response = timeout(TEST_TIMEOUT, ws.next())
        .await
        .expect("Timeout")
        .expect("Stream closed")
        .expect("WebSocket error");

    if let Message::Text(text) = response {
        let server_msg: ServerMessage = serde_json::from_str(&text).unwrap();
        match server_msg {
            ServerMessage::SessionError {
                code,
                message,
                alternatives,
            } => {
                assert_eq!(code, "invalid_voice");
                assert!(message.contains("nonexistent_voice"));
                // Should include the provider's available voice as alternative
                let alts = alternatives.expect("alternatives should be present");
                assert!(alts.contains(&"test_voice".to_string()));
            }
            _ => panic!("Expected SessionError, got: {server_msg:?}"),
        }
    } else {
        panic!("Expected text message");
    }

    ws.close(None).await.ok();
}

/// Test session initialization with invalid voice parameters returns error.
#[tokio::test]
async fn test_websocket_session_init_with_invalid_voice_params() {
    let registry = create_mock_registry();
    let (addr, _handle) = start_test_server(registry).await;

    let mut ws = connect_ws(addr).await;

    // Send session.init with an invalid speed value
    let init_msg = ClientMessage::SessionInit {
        provider: None,
        voice: Some(VoiceConfig {
            id: "test_voice".to_string(),
            speed: 5.0, // Invalid: must be between 0.5 and 2.0
            pitch: 0.0,
            volume: 1.0,
        }),
        audio_format: None,
        code_block_mode: None,
    };
    ws.send(Message::Text(
        serde_json::to_string(&init_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    let response = timeout(TEST_TIMEOUT, ws.next())
        .await
        .expect("Timeout")
        .expect("Stream closed")
        .expect("WebSocket error");

    if let Message::Text(text) = response {
        let server_msg: ServerMessage = serde_json::from_str(&text).unwrap();
        match server_msg {
            ServerMessage::SessionError { code, message, .. } => {
                assert_eq!(code, "invalid_voice_config");
                assert!(message.contains("speed"));
            }
            _ => panic!("Expected SessionError, got: {server_msg:?}"),
        }
    } else {
        panic!("Expected text message");
    }

    ws.close(None).await.ok();
}

/// Test error when no providers are available.
#[tokio::test]
async fn test_websocket_session_init_no_providers() {
    let registry = create_empty_registry();
    let (addr, _handle) = start_test_server(registry).await;

    let mut ws = connect_ws(addr).await;

    // Send session.init
    let init_msg = ClientMessage::SessionInit {
        provider: None,
        voice: None,
        audio_format: None,
        code_block_mode: None,
    };
    ws.send(Message::Text(
        serde_json::to_string(&init_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    // Should receive session.error
    let response = timeout(TEST_TIMEOUT, ws.next())
        .await
        .expect("Timeout")
        .expect("Stream closed")
        .expect("WebSocket error");

    if let Message::Text(text) = response {
        let server_msg: ServerMessage = serde_json::from_str(&text).unwrap();
        match server_msg {
            ServerMessage::SessionError { code, .. } => {
                assert_eq!(code, "no_providers");
            }
            _ => panic!("Expected SessionError, got: {server_msg:?}"),
        }
    } else {
        panic!("Expected text message");
    }

    ws.close(None).await.ok();
}

/// Test error when requested provider does not exist.
#[tokio::test]
async fn test_websocket_session_init_provider_not_found() {
    let registry = create_mock_registry();
    let (addr, _handle) = start_test_server(registry).await;

    let mut ws = connect_ws(addr).await;

    // Request nonexistent provider
    let init_msg = ClientMessage::SessionInit {
        provider: Some("nonexistent".to_string()),
        voice: None,
        audio_format: None,
        code_block_mode: None,
    };
    ws.send(Message::Text(
        serde_json::to_string(&init_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    let response = timeout(TEST_TIMEOUT, ws.next())
        .await
        .expect("Timeout")
        .expect("Stream closed")
        .expect("WebSocket error");

    if let Message::Text(text) = response {
        let server_msg: ServerMessage = serde_json::from_str(&text).unwrap();
        match server_msg {
            ServerMessage::SessionError {
                code, alternatives, ..
            } => {
                assert_eq!(code, "provider_unavailable");
                // Should include mock as alternative
                assert!(alternatives.is_some());
                let alts = alternatives.unwrap();
                assert!(alts.contains(&"mock".to_string()));
            }
            _ => panic!("Expected SessionError, got: {server_msg:?}"),
        }
    } else {
        panic!("Expected text message");
    }

    ws.close(None).await.ok();
}

// ============================================================================
// Full TTS Flow Tests
// ============================================================================

/// Test full TTS flow: init -> text -> audio -> text.done -> audio.done
#[tokio::test]
async fn test_websocket_full_tts_flow() {
    let registry = create_mock_registry();
    let (addr, _handle) = start_test_server(registry).await;

    let mut ws = connect_ws(addr).await;

    // Step 1: Initialize session
    let init_msg = ClientMessage::SessionInit {
        provider: None,
        voice: None,
        audio_format: None,
        code_block_mode: None,
    };
    ws.send(Message::Text(
        serde_json::to_string(&init_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    let response = timeout(TEST_TIMEOUT, ws.next())
        .await
        .expect("Timeout")
        .expect("Stream closed")
        .expect("WebSocket error");
    assert!(
        matches!(response, Message::Text(_)),
        "Expected session.ready"
    );

    // Step 2: Send text with a complete sentence
    let text_msg = ClientMessage::Text {
        content: "Hello, world. This is a test.".to_string(),
    };
    ws.send(Message::Text(
        serde_json::to_string(&text_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    // Step 3: Receive binary audio frames (2 sentences = 2 frames with default mock)
    let mut audio_frames = Vec::new();
    for _ in 0..2 {
        let response = timeout(TEST_TIMEOUT, ws.next())
            .await
            .expect("Timeout waiting for audio")
            .expect("Stream closed")
            .expect("WebSocket error");

        if let Message::Binary(data) = response {
            audio_frames.push(data);
        } else {
            panic!("Expected binary audio frame, got: {response:?}");
        }
    }

    // Verify we got 2 audio frames
    assert_eq!(audio_frames.len(), 2);

    // Verify binary frame format (12-byte header)
    for (i, frame) in audio_frames.iter().enumerate() {
        assert!(frame.len() >= 12, "Frame too short: {} bytes", frame.len());

        // Parse header
        let sequence = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]);
        let sentence_index = u32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]]);
        let duration_ms = u32::from_le_bytes([frame[8], frame[9], frame[10], frame[11]]);

        #[allow(clippy::cast_possible_truncation)]
        let expected_idx = i as u32;
        assert_eq!(sequence, expected_idx, "Wrong sequence number");
        assert_eq!(sentence_index, expected_idx, "Wrong sentence index");
        assert_eq!(duration_ms, 100, "Wrong duration");

        // Verify audio data is the mock pattern (0xAB)
        let audio_data = &frame[12..];
        assert_eq!(audio_data.len(), 256);
        assert!(audio_data.iter().all(|&b| b == 0xAB), "Wrong audio data");
    }

    // Step 4: Send text.done
    let done_msg = ClientMessage::TextDone;
    ws.send(Message::Text(
        serde_json::to_string(&done_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    // Step 5: Receive audio.done
    let response = timeout(TEST_TIMEOUT, ws.next())
        .await
        .expect("Timeout")
        .expect("Stream closed")
        .expect("WebSocket error");

    if let Message::Text(text) = response {
        let server_msg: ServerMessage = serde_json::from_str(&text).unwrap();
        match server_msg {
            ServerMessage::AudioDone {
                total_sentences,
                total_duration_ms,
                total_bytes,
            } => {
                assert_eq!(total_sentences, 2);
                assert_eq!(total_duration_ms, 200); // 2 * 100ms
                assert_eq!(total_bytes, 512); // 2 * 256 bytes
            }
            _ => panic!("Expected AudioDone, got: {server_msg:?}"),
        }
    } else {
        panic!("Expected text message for audio.done");
    }

    ws.close(None).await.ok();
}

/// Test TTS flow with incomplete sentence that gets flushed on text.done.
#[tokio::test]
async fn test_websocket_text_done_flushes_buffer() {
    let registry = create_mock_registry();
    let (addr, _handle) = start_test_server(registry).await;

    let mut ws = connect_ws(addr).await;

    // Initialize session
    let init_msg = ClientMessage::SessionInit {
        provider: None,
        voice: None,
        audio_format: None,
        code_block_mode: None,
    };
    ws.send(Message::Text(
        serde_json::to_string(&init_msg).unwrap().into(),
    ))
    .await
    .unwrap();
    let _ = timeout(TEST_TIMEOUT, ws.next()).await; // Consume session.ready

    // Send text with one complete sentence and one incomplete
    let text_msg = ClientMessage::Text {
        content: "Complete sentence. Incomplete".to_string(),
    };
    ws.send(Message::Text(
        serde_json::to_string(&text_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    // Receive audio for the complete sentence
    let response = timeout(TEST_TIMEOUT, ws.next())
        .await
        .expect("Timeout")
        .expect("Stream closed")
        .expect("WebSocket error");
    assert!(
        matches!(response, Message::Binary(_)),
        "Expected binary for first sentence"
    );

    // Send text.done - should flush the incomplete text
    let done_msg = ClientMessage::TextDone;
    ws.send(Message::Text(
        serde_json::to_string(&done_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    // Should receive audio for flushed content
    let response = timeout(TEST_TIMEOUT, ws.next())
        .await
        .expect("Timeout")
        .expect("Stream closed")
        .expect("WebSocket error");
    assert!(
        matches!(response, Message::Binary(_)),
        "Expected binary for flushed content"
    );

    // Then receive audio.done
    let response = timeout(TEST_TIMEOUT, ws.next())
        .await
        .expect("Timeout")
        .expect("Stream closed")
        .expect("WebSocket error");

    if let Message::Text(text) = response {
        let server_msg: ServerMessage = serde_json::from_str(&text).unwrap();
        match server_msg {
            ServerMessage::AudioDone {
                total_sentences, ..
            } => {
                // 1 complete + 1 flushed = 2 sentences
                assert_eq!(total_sentences, 2);
            }
            _ => panic!("Expected AudioDone"),
        }
    } else {
        panic!("Expected text message");
    }

    ws.close(None).await.ok();
}

/// Test multiple audio chunks per sentence.
#[tokio::test]
async fn test_websocket_multiple_chunks_per_sentence() {
    let registry = create_multi_chunk_registry();
    let (addr, _handle) = start_test_server(registry).await;

    let mut ws = connect_ws(addr).await;

    // Initialize session
    let init_msg = ClientMessage::SessionInit {
        provider: None,
        voice: None,
        audio_format: None,
        code_block_mode: None,
    };
    ws.send(Message::Text(
        serde_json::to_string(&init_msg).unwrap().into(),
    ))
    .await
    .unwrap();
    let _ = timeout(TEST_TIMEOUT, ws.next()).await; // Consume session.ready

    // Send one complete sentence
    let text_msg = ClientMessage::Text {
        content: "One sentence.".to_string(),
    };
    ws.send(Message::Text(
        serde_json::to_string(&text_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    // Should receive 3 audio chunks (configured in multi-chunk registry)
    let mut chunks = Vec::new();
    for _ in 0..3 {
        let response = timeout(TEST_TIMEOUT, ws.next())
            .await
            .expect("Timeout")
            .expect("Stream closed")
            .expect("WebSocket error");

        if let Message::Binary(data) = response {
            chunks.push(data);
        } else {
            panic!("Expected binary frame");
        }
    }

    assert_eq!(chunks.len(), 3);

    // Verify all chunks have same sentence_index but different sequence
    for (i, chunk) in chunks.iter().enumerate() {
        let sequence = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        let sentence_index = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);

        #[allow(clippy::cast_possible_truncation)]
        let expected_seq = i as u32;
        assert_eq!(sequence, expected_seq);
        assert_eq!(sentence_index, 0, "All chunks should be for sentence 0");
    }

    ws.close(None).await.ok();
}

// ============================================================================
// Error Handling Tests
// ============================================================================

/// Test error handling for invalid JSON message.
#[tokio::test]
async fn test_websocket_invalid_json() {
    let registry = create_mock_registry();
    let (addr, _handle) = start_test_server(registry).await;

    let mut ws = connect_ws(addr).await;

    // Send invalid JSON
    ws.send(Message::Text("not valid json".into()))
        .await
        .unwrap();

    let response = timeout(TEST_TIMEOUT, ws.next())
        .await
        .expect("Timeout")
        .expect("Stream closed")
        .expect("WebSocket error");

    if let Message::Text(text) = response {
        let server_msg: ServerMessage = serde_json::from_str(&text).unwrap();
        match server_msg {
            ServerMessage::Error { code, fatal, .. } => {
                assert_eq!(code, "invalid_message");
                assert!(!fatal, "Invalid message should not be fatal");
            }
            _ => panic!("Expected Error, got: {server_msg:?}"),
        }
    } else {
        panic!("Expected text message");
    }

    ws.close(None).await.ok();
}

/// Test error handling for text message before session.init.
#[tokio::test]
async fn test_websocket_text_without_session() {
    let registry = create_mock_registry();
    let (addr, _handle) = start_test_server(registry).await;

    let mut ws = connect_ws(addr).await;

    // Send text without session.init
    let text_msg = ClientMessage::Text {
        content: "Hello".to_string(),
    };
    ws.send(Message::Text(
        serde_json::to_string(&text_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    let response = timeout(TEST_TIMEOUT, ws.next())
        .await
        .expect("Timeout")
        .expect("Stream closed")
        .expect("WebSocket error");

    if let Message::Text(text) = response {
        let server_msg: ServerMessage = serde_json::from_str(&text).unwrap();
        match server_msg {
            ServerMessage::Error { code, .. } => {
                assert_eq!(code, "no_session");
            }
            _ => panic!("Expected Error, got: {server_msg:?}"),
        }
    } else {
        panic!("Expected text message");
    }

    ws.close(None).await.ok();
}

/// Test error handling for text.done before session.init.
#[tokio::test]
async fn test_websocket_text_done_without_session() {
    let registry = create_mock_registry();
    let (addr, _handle) = start_test_server(registry).await;

    let mut ws = connect_ws(addr).await;

    // Send text.done without session
    let done_msg = ClientMessage::TextDone;
    ws.send(Message::Text(
        serde_json::to_string(&done_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    let response = timeout(TEST_TIMEOUT, ws.next())
        .await
        .expect("Timeout")
        .expect("Stream closed")
        .expect("WebSocket error");

    if let Message::Text(text) = response {
        let server_msg: ServerMessage = serde_json::from_str(&text).unwrap();
        match server_msg {
            ServerMessage::Error { code, .. } => {
                assert_eq!(code, "no_session");
            }
            _ => panic!("Expected Error"),
        }
    } else {
        panic!("Expected text message");
    }

    ws.close(None).await.ok();
}

/// Test error handling for duplicate session.init.
#[tokio::test]
async fn test_websocket_duplicate_session_init() {
    let registry = create_mock_registry();
    let (addr, _handle) = start_test_server(registry).await;

    let mut ws = connect_ws(addr).await;

    // First session.init
    let init_msg = ClientMessage::SessionInit {
        provider: None,
        voice: None,
        audio_format: None,
        code_block_mode: None,
    };
    ws.send(Message::Text(
        serde_json::to_string(&init_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    let response = timeout(TEST_TIMEOUT, ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(response, Message::Text(_)));

    // Second session.init (should fail)
    ws.send(Message::Text(
        serde_json::to_string(&init_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    let response = timeout(TEST_TIMEOUT, ws.next())
        .await
        .expect("Timeout")
        .expect("Stream closed")
        .expect("WebSocket error");

    if let Message::Text(text) = response {
        let server_msg: ServerMessage = serde_json::from_str(&text).unwrap();
        match server_msg {
            ServerMessage::Error { code, .. } => {
                assert_eq!(code, "session_already_initialized");
            }
            _ => panic!("Expected Error, got: {server_msg:?}"),
        }
    } else {
        panic!("Expected text message");
    }

    ws.close(None).await.ok();
}

/// Test error handling for unexpected binary message from client.
#[tokio::test]
async fn test_websocket_unexpected_binary() {
    let registry = create_mock_registry();
    let (addr, _handle) = start_test_server(registry).await;

    let mut ws = connect_ws(addr).await;

    // Send binary data (not expected from client)
    ws.send(Message::Binary(vec![0u8; 100].into()))
        .await
        .unwrap();

    let response = timeout(TEST_TIMEOUT, ws.next())
        .await
        .expect("Timeout")
        .expect("Stream closed")
        .expect("WebSocket error");

    if let Message::Text(text) = response {
        let server_msg: ServerMessage = serde_json::from_str(&text).unwrap();
        match server_msg {
            ServerMessage::Error { code, .. } => {
                assert_eq!(code, "unexpected_binary");
            }
            _ => panic!("Expected Error"),
        }
    } else {
        panic!("Expected text message");
    }

    ws.close(None).await.ok();
}

// ============================================================================
// Binary Audio Frame Format Tests
// ============================================================================

/// Test binary audio frame parsing using `AudioChunk::from_binary_frame`.
#[tokio::test]
async fn test_websocket_binary_frame_roundtrip() {
    use yappy_core::AudioChunk;

    let registry = create_mock_registry();
    let (addr, _handle) = start_test_server(registry).await;

    let mut ws = connect_ws(addr).await;

    // Initialize session
    let init_msg = ClientMessage::SessionInit {
        provider: None,
        voice: None,
        audio_format: None,
        code_block_mode: None,
    };
    ws.send(Message::Text(
        serde_json::to_string(&init_msg).unwrap().into(),
    ))
    .await
    .unwrap();
    let _ = timeout(TEST_TIMEOUT, ws.next()).await; // Consume session.ready

    // Send text
    let text_msg = ClientMessage::Text {
        content: "Test sentence.".to_string(),
    };
    ws.send(Message::Text(
        serde_json::to_string(&text_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    // Receive binary frame
    let response = timeout(TEST_TIMEOUT, ws.next())
        .await
        .expect("Timeout")
        .expect("Stream closed")
        .expect("WebSocket error");

    if let Message::Binary(data) = response {
        // Use AudioChunk::from_binary_frame to parse
        let frame_bytes = Bytes::from(data.to_vec());
        let chunk = AudioChunk::from_binary_frame(&frame_bytes).expect("Failed to parse frame");

        assert_eq!(chunk.sequence, 0);
        assert_eq!(chunk.sentence_index, 0);
        assert_eq!(chunk.duration_ms, 100);
        assert_eq!(chunk.data.len(), 256);
        assert!(chunk.data.iter().all(|&b| b == 0xAB));
    } else {
        panic!("Expected binary frame");
    }

    ws.close(None).await.ok();
}

// ============================================================================
// Connection Lifecycle Tests
// ============================================================================

/// Test that server handles clean WebSocket close.
#[tokio::test]
async fn test_websocket_clean_close() {
    let registry = create_mock_registry();
    let (addr, _handle) = start_test_server(registry).await;

    let mut ws = connect_ws(addr).await;

    // Initialize session
    let init_msg = ClientMessage::SessionInit {
        provider: None,
        voice: None,
        audio_format: None,
        code_block_mode: None,
    };
    ws.send(Message::Text(
        serde_json::to_string(&init_msg).unwrap().into(),
    ))
    .await
    .unwrap();
    let _ = timeout(TEST_TIMEOUT, ws.next()).await;

    // Clean close
    ws.close(None).await.expect("Close should succeed");
}

/// Test that session can complete multiple sentences.
#[tokio::test]
async fn test_websocket_multiple_text_messages() {
    let registry = create_mock_registry();
    let (addr, _handle) = start_test_server(registry).await;

    let mut ws = connect_ws(addr).await;

    // Initialize session
    let init_msg = ClientMessage::SessionInit {
        provider: None,
        voice: None,
        audio_format: None,
        code_block_mode: None,
    };
    ws.send(Message::Text(
        serde_json::to_string(&init_msg).unwrap().into(),
    ))
    .await
    .unwrap();
    let _ = timeout(TEST_TIMEOUT, ws.next()).await;

    // Send multiple text messages
    for i in 0..3 {
        let text_msg = ClientMessage::Text {
            content: format!("Sentence {i}."),
        };
        ws.send(Message::Text(
            serde_json::to_string(&text_msg).unwrap().into(),
        ))
        .await
        .unwrap();

        // Receive audio for each
        let response = timeout(TEST_TIMEOUT, ws.next())
            .await
            .expect("Timeout")
            .expect("Stream closed")
            .expect("WebSocket error");
        assert!(matches!(response, Message::Binary(_)));
    }

    // Send text.done
    let done_msg = ClientMessage::TextDone;
    ws.send(Message::Text(
        serde_json::to_string(&done_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    // Receive audio.done
    let response = timeout(TEST_TIMEOUT, ws.next())
        .await
        .expect("Timeout")
        .expect("Stream closed")
        .expect("WebSocket error");

    if let Message::Text(text) = response {
        let server_msg: ServerMessage = serde_json::from_str(&text).unwrap();
        match server_msg {
            ServerMessage::AudioDone {
                total_sentences, ..
            } => {
                assert_eq!(total_sentences, 3);
            }
            _ => panic!("Expected AudioDone"),
        }
    }

    ws.close(None).await.ok();
}

/// Test audio.done statistics accumulation.
#[tokio::test]
async fn test_websocket_audio_done_statistics() {
    let registry = create_multi_chunk_registry();
    let (addr, _handle) = start_test_server(registry).await;

    let mut ws = connect_ws(addr).await;

    // Initialize session
    let init_msg = ClientMessage::SessionInit {
        provider: None,
        voice: None,
        audio_format: None,
        code_block_mode: None,
    };
    ws.send(Message::Text(
        serde_json::to_string(&init_msg).unwrap().into(),
    ))
    .await
    .unwrap();
    let _ = timeout(TEST_TIMEOUT, ws.next()).await;

    // Send 2 complete sentences
    let text_msg = ClientMessage::Text {
        content: "First sentence. Second sentence.".to_string(),
    };
    ws.send(Message::Text(
        serde_json::to_string(&text_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    // Multi-chunk registry: 3 chunks per sentence, 2 sentences = 6 chunks
    for _ in 0..6 {
        let response = timeout(TEST_TIMEOUT, ws.next())
            .await
            .expect("Timeout")
            .expect("Stream closed")
            .expect("WebSocket error");
        assert!(matches!(response, Message::Binary(_)));
    }

    // Send text.done
    let done_msg = ClientMessage::TextDone;
    ws.send(Message::Text(
        serde_json::to_string(&done_msg).unwrap().into(),
    ))
    .await
    .unwrap();

    // Receive audio.done with statistics
    let response = timeout(TEST_TIMEOUT, ws.next())
        .await
        .expect("Timeout")
        .expect("Stream closed")
        .expect("WebSocket error");

    if let Message::Text(text) = response {
        let server_msg: ServerMessage = serde_json::from_str(&text).unwrap();
        match server_msg {
            ServerMessage::AudioDone {
                total_sentences,
                total_duration_ms,
                total_bytes,
            } => {
                assert_eq!(total_sentences, 2);
                // 6 chunks * 50ms = 300ms
                assert_eq!(total_duration_ms, 300);
                // 6 chunks * 128 bytes = 768 bytes
                assert_eq!(total_bytes, 768);
            }
            _ => panic!("Expected AudioDone"),
        }
    }

    ws.close(None).await.ok();
}
