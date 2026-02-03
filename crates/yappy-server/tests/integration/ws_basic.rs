//! Basic WebSocket integration tests for the Yappy TTS server
//!
//! These tests verify the core WebSocket TTS flow:
//! - Connection and session initialization
//! - Text streaming and audio reception
//! - Session completion
//! - Error handling

use std::time::Duration;

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

use yappy_core::{AudioChunk, ClientMessage, ServerMessage};

use crate::common::{
    connect_ws, finish_session, init_session, receive_audio_chunk_with_timeout,
    receive_server_message_with_timeout, send_message, spawn_empty_server, spawn_test_server,
    MockSynthesisMode, MockTtsProvider, TestServerBuilder, DEFAULT_TIMEOUT,
};

// ============================================================================
// Connection and Session Initialization Tests
// ============================================================================

/// Test: Connect to WebSocket and initialize a session successfully
///
/// Flow:
/// 1. Connect to /ws endpoint
/// 2. Send session.init message
/// 3. Receive session.ready response
/// 4. Verify `session_id` is returned
#[tokio::test]
async fn test_ws_connect_and_session_init() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Send session.init
    let init_msg = ClientMessage::SessionInit {
        provider: Some("mock".to_string()),
        voice: None,
        audio_format: None,
        code_block_mode: None,
    };
    send_message(&mut ws, &init_msg)
        .await
        .expect("Should send init message");

    // Receive session.ready
    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::SessionReady {
            session_id,
            voice,
            audio_format,
        } => {
            // Session ID should follow expected format
            assert!(
                session_id.starts_with("ses_"),
                "Session ID should start with 'ses_'"
            );
            // Voice should be one of the mock provider's voices
            assert!(
                voice == "test_voice_1" || voice == "test_voice_2",
                "Unexpected voice: {voice}"
            );
            // Audio format should be set
            assert_eq!(audio_format.sample_rate, 48000);
        }
        other => panic!("Expected SessionReady, got: {other:?}"),
    }

    server.shutdown();
}

/// Test: Session initialization with default provider (no provider specified)
#[tokio::test]
async fn test_ws_session_init_default_provider() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Use the helper which doesn't specify a provider
    let session_id = init_session(&mut ws, None)
        .await
        .expect("Should initialize session with default provider");

    assert!(session_id.starts_with("ses_"));

    server.shutdown();
}

/// Test: Session initialization with specific provider
#[tokio::test]
async fn test_ws_session_init_specific_provider() {
    let server = TestServerBuilder::new()
        .with_mock_provider("provider_a")
        .with_mock_provider("provider_b")
        .with_default_provider("provider_a")
        .spawn()
        .await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Request specific provider
    let session_id = init_session(&mut ws, Some("provider_b"))
        .await
        .expect("Should initialize session with provider_b");

    assert!(session_id.starts_with("ses_"));

    server.shutdown();
}

// ============================================================================
// Full TTS Flow Tests
// ============================================================================

/// Test: Full TTS flow - init, text, audio, done
///
/// Flow:
/// 1. Connect and init session
/// 2. Send text with complete sentences
/// 3. Receive binary audio frames
/// 4. Send text.done
/// 5. Receive audio.done with statistics
#[tokio::test]
#[allow(clippy::cast_possible_truncation)]
async fn test_ws_full_tts_flow() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Initialize session
    let session_id = init_session(&mut ws, Some("mock"))
        .await
        .expect("Should initialize session");
    assert!(session_id.starts_with("ses_"));

    // Send text with complete sentences
    let text_msg = ClientMessage::Text {
        content: "Hello world. This is a test.".to_string(),
    };
    send_message(&mut ws, &text_msg)
        .await
        .expect("Should send text");

    // Collect audio chunks (expect 2 for 2 sentences)
    let mut audio_chunks = Vec::new();
    for _ in 0..10 {
        match timeout(Duration::from_millis(500), ws.next()).await {
            Ok(Some(Ok(Message::Binary(data)))) => {
                if let Some(chunk) = AudioChunk::from_binary_frame(&Bytes::from(data.to_vec())) {
                    audio_chunks.push(chunk);
                }
            }
            _ => break,
        }
    }

    // Should have received 2 audio chunks (one per sentence)
    assert_eq!(audio_chunks.len(), 2, "Expected 2 audio chunks");

    // Verify audio chunk structure
    for (i, chunk) in audio_chunks.iter().enumerate() {
        assert_eq!(chunk.sequence, i as u32, "Sequence number mismatch");
        assert_eq!(chunk.sentence_index, i as u32, "Sentence index mismatch");
        assert!(chunk.duration_ms > 0, "Duration should be positive");
        assert!(!chunk.data.is_empty(), "Audio data should not be empty");
    }

    // Send text.done and wait for audio.done
    let (total_sentences, total_duration_ms, total_bytes) = finish_session(&mut ws)
        .await
        .expect("Should finish session");

    assert_eq!(total_sentences, 2, "Should have 2 sentences");
    assert!(total_duration_ms > 0, "Duration should be positive");
    assert!(total_bytes > 0, "Bytes should be positive");

    server.shutdown();
}

/// Test: TTS flow with incomplete sentence that gets flushed on text.done
#[tokio::test]
async fn test_ws_tts_flow_with_flush() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Initialize session
    init_session(&mut ws, Some("mock"))
        .await
        .expect("Should initialize session");

    // Send text with incomplete sentence (no period at end)
    let text_msg = ClientMessage::Text {
        content: "Complete sentence. Incomplete".to_string(),
    };
    send_message(&mut ws, &text_msg)
        .await
        .expect("Should send text");

    // Collect audio for the complete sentence
    let mut audio_count = 0;
    for _ in 0..5 {
        match timeout(Duration::from_millis(300), ws.next()).await {
            Ok(Some(Ok(Message::Binary(_)))) => audio_count += 1,
            _ => break,
        }
    }
    assert_eq!(
        audio_count, 1,
        "Should have 1 audio chunk for complete sentence"
    );

    // Send text.done - should flush "Incomplete" and synthesize it
    send_message(&mut ws, &ClientMessage::TextDone)
        .await
        .expect("Should send text.done");

    // Should receive audio for the flushed incomplete sentence
    let mut received_flushed_audio = false;
    let mut audio_done_received = false;

    for _ in 0..10 {
        match timeout(Duration::from_millis(500), ws.next()).await {
            Ok(Some(Ok(Message::Binary(_)))) => {
                received_flushed_audio = true;
            }
            Ok(Some(Ok(Message::Text(text)))) => {
                let msg: ServerMessage = serde_json::from_str(&text).expect("Should parse message");
                if let ServerMessage::AudioDone {
                    total_sentences, ..
                } = msg
                {
                    audio_done_received = true;
                    assert_eq!(
                        total_sentences, 2,
                        "Should have 2 sentences (1 complete + 1 flushed)"
                    );
                    break;
                }
            }
            _ => break,
        }
    }

    assert!(
        received_flushed_audio,
        "Should receive audio for flushed sentence"
    );
    assert!(audio_done_received, "Should receive audio.done");

    server.shutdown();
}

/// Test: Multiple text messages building up content
#[tokio::test]
async fn test_ws_multiple_text_messages() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    init_session(&mut ws, Some("mock"))
        .await
        .expect("Should initialize session");

    // Send text incrementally
    send_message(
        &mut ws,
        &ClientMessage::Text {
            content: "First ".to_string(),
        },
    )
    .await
    .expect("Should send text");

    send_message(
        &mut ws,
        &ClientMessage::Text {
            content: "sentence. Second ".to_string(),
        },
    )
    .await
    .expect("Should send text");

    send_message(
        &mut ws,
        &ClientMessage::Text {
            content: "sentence.".to_string(),
        },
    )
    .await
    .expect("Should send text");

    // Collect audio chunks
    let mut audio_count = 0;
    for _ in 0..10 {
        match timeout(Duration::from_millis(300), ws.next()).await {
            Ok(Some(Ok(Message::Binary(_)))) => audio_count += 1,
            _ => break,
        }
    }

    // Should have 2 chunks for 2 sentences
    assert_eq!(audio_count, 2, "Should have 2 audio chunks");

    // Finish session
    let (total_sentences, _, _) = finish_session(&mut ws)
        .await
        .expect("Should finish session");
    assert_eq!(total_sentences, 2);

    server.shutdown();
}

// ============================================================================
// Error Handling Tests
// ============================================================================

/// Test: Session init error when no providers are available
#[tokio::test]
async fn test_ws_session_init_error_no_provider() {
    let server = spawn_empty_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Try to initialize session
    let init_msg = ClientMessage::SessionInit {
        provider: None,
        voice: None,
        audio_format: None,
        code_block_mode: None,
    };
    send_message(&mut ws, &init_msg)
        .await
        .expect("Should send init message");

    // Should receive session.error
    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::SessionError { code, message, .. } => {
            assert_eq!(code, "no_providers");
            assert!(message.contains("No TTS providers"));
        }
        other => panic!("Expected SessionError, got: {other:?}"),
    }

    // Connection should be closed after fatal error
    // Try to receive - should get close or error
    let next = timeout(Duration::from_millis(500), ws.next()).await;
    match next {
        Ok(Some(Ok(Message::Close(_)) | Err(_)) | None) | Err(_) => {
            // Expected - connection closed or error
        }
        Ok(Some(Ok(other))) => {
            panic!("Expected connection close, got: {other:?}");
        }
    }

    server.shutdown();
}

/// Test: Requesting a non-existent provider
#[tokio::test]
async fn test_ws_session_init_error_provider_not_found() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Request a provider that doesn't exist
    let init_msg = ClientMessage::SessionInit {
        provider: Some("nonexistent_provider".to_string()),
        voice: None,
        audio_format: None,
        code_block_mode: None,
    };
    send_message(&mut ws, &init_msg)
        .await
        .expect("Should send init message");

    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::SessionError {
            code, alternatives, ..
        } => {
            assert_eq!(code, "provider_unavailable");
            // Should include available alternatives
            let alts = alternatives.expect("Should have alternatives");
            assert!(alts.contains(&"mock".to_string()));
        }
        other => panic!("Expected SessionError, got: {other:?}"),
    }

    server.shutdown();
}

/// Test: Sending text before session.init
#[tokio::test]
async fn test_ws_text_without_session() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Send text without initializing session first
    let text_msg = ClientMessage::Text {
        content: "Hello world.".to_string(),
    };
    send_message(&mut ws, &text_msg)
        .await
        .expect("Should send text message");

    // Should receive an error
    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::Error { code, fatal, .. } => {
            assert_eq!(code, "no_session");
            assert!(!fatal, "Should be non-fatal error");
        }
        other => panic!("Expected Error, got: {other:?}"),
    }

    // Connection should still be open (non-fatal error)
    // Now we can init session and continue
    init_session(&mut ws, Some("mock"))
        .await
        .expect("Should be able to init session after error");

    server.shutdown();
}

/// Test: Sending text.done before session.init
#[tokio::test]
async fn test_ws_text_done_without_session() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Send text.done without initializing session
    send_message(&mut ws, &ClientMessage::TextDone)
        .await
        .expect("Should send text.done");

    // Should receive an error
    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::Error { code, fatal, .. } => {
            assert_eq!(code, "no_session");
            assert!(!fatal);
        }
        other => panic!("Expected Error, got: {other:?}"),
    }

    server.shutdown();
}

/// Test: Double session.init should return error
#[tokio::test]
async fn test_ws_double_session_init() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // First init should succeed
    init_session(&mut ws, Some("mock"))
        .await
        .expect("First init should succeed");

    // Second init should return error
    let init_msg = ClientMessage::SessionInit {
        provider: Some("mock".to_string()),
        voice: None,
        audio_format: None,
        code_block_mode: None,
    };
    send_message(&mut ws, &init_msg)
        .await
        .expect("Should send second init");

    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::Error { code, fatal, .. } => {
            assert_eq!(code, "session_already_initialized");
            assert!(!fatal);
        }
        other => panic!("Expected Error, got: {other:?}"),
    }

    server.shutdown();
}

/// Test: Invalid JSON message
#[tokio::test]
async fn test_ws_invalid_json() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Send invalid JSON
    ws.send(Message::Text("not valid json".into()))
        .await
        .expect("Should send message");

    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::Error { code, fatal, .. } => {
            assert_eq!(code, "invalid_message");
            assert!(!fatal);
        }
        other => panic!("Expected Error, got: {other:?}"),
    }

    server.shutdown();
}

/// Test: Binary frame from client (not supported)
#[tokio::test]
async fn test_ws_unexpected_binary() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Send binary frame (clients should only send text)
    ws.send(Message::Binary(vec![1, 2, 3, 4].into()))
        .await
        .expect("Should send binary");

    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::Error { code, fatal, .. } => {
            assert_eq!(code, "unexpected_binary");
            assert!(!fatal);
        }
        other => panic!("Expected Error, got: {other:?}"),
    }

    server.shutdown();
}

// ============================================================================
// Provider-Specific Tests
// ============================================================================

/// Test: Provider synthesis error is handled gracefully
#[tokio::test]
async fn test_ws_synthesis_error() {
    // Create a provider that fails during synthesis
    let failing_provider =
        MockTtsProvider::new("failing").with_synthesis_mode(MockSynthesisMode::FailInit {
            error: "Test synthesis failure".to_string(),
        });

    let server = TestServerBuilder::new()
        .with_provider(failing_provider)
        .with_default_provider("failing")
        .spawn()
        .await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    init_session(&mut ws, Some("failing"))
        .await
        .expect("Should initialize session");

    // Send text with a complete sentence
    let text_msg = ClientMessage::Text {
        content: "Hello world.".to_string(),
    };
    send_message(&mut ws, &text_msg)
        .await
        .expect("Should send text");

    // Should receive an error for the failed synthesis
    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::Error {
            code,
            fatal,
            sentence_index,
            ..
        } => {
            assert_eq!(code, "synthesis_failed");
            assert!(!fatal);
            assert_eq!(sentence_index, Some(0));
        }
        other => panic!("Expected Error, got: {other:?}"),
    }

    // Session should still be open - we can send text.done
    let (total_sentences, _, _) = finish_session(&mut ws)
        .await
        .expect("Should finish session");

    // The failed sentence still counts in the total
    assert_eq!(total_sentences, 1);

    server.shutdown();
}

/// Test: Provider with multiple chunks per sentence
#[tokio::test]
async fn test_ws_multiple_chunks_per_sentence() {
    let multi_chunk_provider =
        MockTtsProvider::new("multi").with_synthesis_mode(MockSynthesisMode::Success {
            chunks_per_sentence: 3,
            chunk_duration_ms: 50,
            chunk_bytes: 128,
        });

    let server = TestServerBuilder::new()
        .with_provider(multi_chunk_provider)
        .spawn()
        .await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    init_session(&mut ws, Some("multi"))
        .await
        .expect("Should initialize session");

    // Send text with one sentence
    let text_msg = ClientMessage::Text {
        content: "Single sentence.".to_string(),
    };
    send_message(&mut ws, &text_msg)
        .await
        .expect("Should send text");

    // Should receive 3 audio chunks for the one sentence
    let mut chunks = Vec::new();
    for _ in 0..10 {
        match timeout(Duration::from_millis(300), ws.next()).await {
            Ok(Some(Ok(Message::Binary(data)))) => {
                if let Some(chunk) = AudioChunk::from_binary_frame(&Bytes::from(data.to_vec())) {
                    chunks.push(chunk);
                }
            }
            _ => break,
        }
    }

    assert_eq!(chunks.len(), 3, "Should have 3 chunks");

    // All chunks should have the same sentence_index
    for chunk in &chunks {
        assert_eq!(chunk.sentence_index, 0);
    }

    // Sequence numbers should be unique and sequential
    #[allow(clippy::cast_possible_truncation)]
    for (i, chunk) in chunks.iter().enumerate() {
        assert_eq!(chunk.sequence, i as u32);
    }

    // Finish session
    let (total_sentences, total_duration_ms, total_bytes) = finish_session(&mut ws)
        .await
        .expect("Should finish session");

    assert_eq!(total_sentences, 1);
    assert_eq!(total_duration_ms, 150); // 3 chunks * 50ms
    assert_eq!(total_bytes, 384); // 3 chunks * 128 bytes

    server.shutdown();
}

// ============================================================================
// Connection Lifecycle Tests
// ============================================================================

/// Test: Server handles client disconnect gracefully
#[tokio::test]
async fn test_ws_client_disconnect() {
    let server = spawn_test_server().await;

    // Connect, init, then drop connection
    {
        let mut ws = connect_ws(&server)
            .await
            .expect("Should connect to WebSocket");

        init_session(&mut ws, Some("mock"))
            .await
            .expect("Should initialize session");

        // Drop ws - client disconnects
    }

    // Server should still be running and accept new connections
    let mut ws2 = connect_ws(&server).await.expect("Should connect again");

    init_session(&mut ws2, Some("mock"))
        .await
        .expect("Should initialize new session");

    server.shutdown();
}

/// Test: Multiple concurrent WebSocket connections
#[tokio::test]
async fn test_ws_concurrent_connections() {
    let server = spawn_test_server().await;

    // Connect multiple clients
    let mut ws1 = connect_ws(&server).await.expect("Should connect client 1");
    let mut ws2 = connect_ws(&server).await.expect("Should connect client 2");

    // Both should be able to init sessions
    let session_id_1 = init_session(&mut ws1, Some("mock"))
        .await
        .expect("Client 1 should init");
    let session_id_2 = init_session(&mut ws2, Some("mock"))
        .await
        .expect("Client 2 should init");

    // Session IDs should be different
    assert_ne!(session_id_1, session_id_2);

    // Both should be able to send text and receive audio
    send_message(
        &mut ws1,
        &ClientMessage::Text {
            content: "Client 1.".to_string(),
        },
    )
    .await
    .expect("Client 1 should send");

    send_message(
        &mut ws2,
        &ClientMessage::Text {
            content: "Client 2.".to_string(),
        },
    )
    .await
    .expect("Client 2 should send");

    // Both should receive audio
    let chunk1 = receive_audio_chunk_with_timeout(&mut ws1, DEFAULT_TIMEOUT)
        .await
        .expect("Client 1 should receive audio");
    let chunk2 = receive_audio_chunk_with_timeout(&mut ws2, DEFAULT_TIMEOUT)
        .await
        .expect("Client 2 should receive audio");

    assert!(!chunk1.data.is_empty());
    assert!(!chunk2.data.is_empty());

    // Both should finish successfully
    finish_session(&mut ws1)
        .await
        .expect("Client 1 should finish");
    finish_session(&mut ws2)
        .await
        .expect("Client 2 should finish");

    server.shutdown();
}

/// Test: WebSocket ping/pong (server should respond to pings)
#[tokio::test]
async fn test_ws_ping_pong() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Send a ping
    ws.send(Message::Ping(vec![1, 2, 3].into()))
        .await
        .expect("Should send ping");

    // We may receive a Pong frame back - consume it if present
    // (Axum responds with Pong, and tokio-tungstenite may surface it)
    if let Ok(Some(Ok(Message::Pong(data)))) = timeout(Duration::from_millis(100), ws.next()).await
    {
        // Pong payload should match ping payload
        assert_eq!(data.as_ref(), &[1, 2, 3]);
    } else {
        // No pong received (might be handled internally) - that's also fine
    }

    // Verify connection is still working
    let init_msg = ClientMessage::SessionInit {
        provider: Some("mock".to_string()),
        voice: None,
        audio_format: None,
        code_block_mode: None,
    };
    send_message(&mut ws, &init_msg)
        .await
        .expect("Should send init message");

    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::SessionReady { .. } => {
            // Success - connection still works after ping/pong
        }
        other => panic!("Expected SessionReady, got: {other:?}"),
    }

    server.shutdown();
}
