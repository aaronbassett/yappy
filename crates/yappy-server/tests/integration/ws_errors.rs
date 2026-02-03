//! Error recovery integration tests for the Yappy TTS server
//!
//! These tests verify error handling behavior in the WebSocket TTS flow:
//! - Non-fatal errors (synthesis failures) that allow continuation
//! - Rate limiting errors with retry information
//! - Fatal errors that close the connection (protocol violations)
//! - Error message format verification

use std::time::Duration;

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

use yappy_core::{AudioChunk, ClientMessage, ServerMessage};

use crate::common::{
    connect_ws, finish_session, init_session, receive_server_message_with_timeout, send_message,
    spawn_test_server, MockSynthesisMode, MockTtsProvider, TestServerBuilder, DEFAULT_TIMEOUT,
};

// ============================================================================
// Non-Fatal Error Handling Tests
// ============================================================================

/// Test: Provider fails mid-stream, error is sent with `sentence_index`,
/// and subsequent sentences continue to be processed
///
/// Flow:
/// 1. Connect and init session with a provider that fails mid-stream
/// 2. Send text with multiple sentences
/// 3. First sentence produces some audio chunks before failing
/// 4. Receive error message with `sentence_index`
/// 5. Second sentence is processed successfully
/// 6. Session completes normally
#[tokio::test]
async fn test_nonfatal_error_mid_stream_allows_continuation() {
    // Create a provider that fails mid-stream on the first sentence
    let failing_provider =
        MockTtsProvider::new("mid_fail").with_synthesis_mode(MockSynthesisMode::FailMidStream {
            chunks_before_error: 2,
            error: "Mid-stream synthesis failure".to_string(),
        });

    let server = TestServerBuilder::new()
        .with_provider(failing_provider)
        .with_default_provider("mid_fail")
        .spawn()
        .await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    init_session(&mut ws, Some("mid_fail"))
        .await
        .expect("Should initialize session");

    // Send text with one sentence (the provider fails mid-stream for every sentence)
    // Note: SRX boundary detection requires following text to emit a sentence
    let text_msg = ClientMessage::Text {
        content: "First sentence. More text".to_string(),
    };
    send_message(&mut ws, &text_msg)
        .await
        .expect("Should send text");

    // Collect audio chunks until we get the error
    let mut audio_chunks_before_error = 0;
    let mut received_error = false;
    let mut error_sentence_index = None;

    for _ in 0..10 {
        match timeout(Duration::from_millis(500), ws.next()).await {
            Ok(Some(Ok(Message::Binary(data)))) => {
                if AudioChunk::from_binary_frame(&Bytes::from(data.to_vec())).is_some() {
                    audio_chunks_before_error += 1;
                }
            }
            Ok(Some(Ok(Message::Text(text)))) => {
                let msg: ServerMessage = serde_json::from_str(&text).expect("Should parse message");
                if let ServerMessage::Error {
                    code,
                    fatal,
                    sentence_index,
                    ..
                } = msg
                {
                    assert_eq!(
                        code, "synthesis_failed",
                        "Error code should be synthesis_failed"
                    );
                    assert!(!fatal, "Error should be non-fatal");
                    error_sentence_index = sentence_index;
                    received_error = true;
                    break;
                }
            }
            _ => break,
        }
    }

    assert!(received_error, "Should receive synthesis_failed error");
    assert_eq!(
        audio_chunks_before_error, 2,
        "Should receive 2 audio chunks before error"
    );
    assert_eq!(
        error_sentence_index,
        Some(0),
        "Error should include sentence_index 0"
    );

    // Session should still be open - we can finish normally
    let (total_sentences, _, _) = finish_session(&mut ws)
        .await
        .expect("Should finish session");

    // Both the failed sentence and the flushed "More text" count in the total
    assert_eq!(
        total_sentences, 2,
        "Total sentences should be 2 (1 failed + 1 flushed)"
    );

    server.shutdown();
}

/// Test: Provider synthesis fails at initialization, error is sent with `sentence_index`,
/// and subsequent sentences are still processed
#[tokio::test]
async fn test_nonfatal_error_synthesis_init_failure() {
    // Create a provider that fails at synthesis initialization
    let failing_provider =
        MockTtsProvider::new("init_fail").with_synthesis_mode(MockSynthesisMode::FailInit {
            error: "Provider failed to synthesize".to_string(),
        });

    let server = TestServerBuilder::new()
        .with_provider(failing_provider)
        .with_default_provider("init_fail")
        .spawn()
        .await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    init_session(&mut ws, Some("init_fail"))
        .await
        .expect("Should initialize session");

    // Send text with a complete sentence
    // Note: SRX boundary detection requires following text to emit a sentence
    let text_msg = ClientMessage::Text {
        content: "Test sentence. More text".to_string(),
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
            message,
            fatal,
            sentence_index,
        } => {
            assert_eq!(
                code, "synthesis_failed",
                "Error code should be synthesis_failed"
            );
            assert!(
                message.contains("Provider failed to synthesize"),
                "Error message should contain the provider error"
            );
            assert!(!fatal, "Error should be non-fatal");
            assert_eq!(
                sentence_index,
                Some(0),
                "Error should include sentence_index"
            );
        }
        other => panic!("Expected Error, got: {other:?}"),
    }

    // Session should still be open - we can send more text and finish
    // The trailing "More text" fragment is flushed as a 2nd sentence on text.done
    let (total_sentences, _, _) = finish_session(&mut ws)
        .await
        .expect("Should finish session");

    assert_eq!(
        total_sentences, 2,
        "Total sentences should be 2 (1 failed + 1 flushed)"
    );

    server.shutdown();
}

// ============================================================================
// Rate Limit Error Handling Tests
// ============================================================================

/// Test: Provider returns `rate_limited` error, which is non-fatal and includes retry info
///
/// Flow:
/// 1. Connect and init session with a rate-limited provider
/// 2. Send text
/// 3. Receive `rate_limited` error with `retry_after` information
/// 4. Session remains open
#[tokio::test]
async fn test_rate_limit_error_is_nonfatal_with_retry_info() {
    // Create a provider that returns rate limit error
    let rate_limited_provider =
        MockTtsProvider::new("rate_limited").with_synthesis_mode(MockSynthesisMode::RateLimited {
            retry_after_secs: 30,
        });

    let server = TestServerBuilder::new()
        .with_provider(rate_limited_provider)
        .with_default_provider("rate_limited")
        .spawn()
        .await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    init_session(&mut ws, Some("rate_limited"))
        .await
        .expect("Should initialize session");

    // Send text
    // Note: SRX boundary detection requires following text to emit a sentence
    let text_msg = ClientMessage::Text {
        content: "Test sentence. More text".to_string(),
    };
    send_message(&mut ws, &text_msg)
        .await
        .expect("Should send text");

    // Should receive rate_limited error
    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::Error {
            code,
            message,
            fatal,
            sentence_index,
        } => {
            assert_eq!(code, "rate_limited", "Error code should be rate_limited");
            assert!(
                message.contains("30"),
                "Error message should include retry_after information: {message}"
            );
            assert!(!fatal, "Rate limit error should be non-fatal");
            assert_eq!(
                sentence_index,
                Some(0),
                "Error should include sentence_index"
            );
        }
        other => panic!("Expected Error with rate_limited code, got: {other:?}"),
    }

    // Session should still be open - we can finish normally
    let result = finish_session(&mut ws).await;
    assert!(
        result.is_ok(),
        "Should be able to finish session after rate limit error"
    );

    server.shutdown();
}

// ============================================================================
// Fatal Error Handling Tests
// ============================================================================

/// Test: Sending text before session.init is a protocol violation
/// (In current implementation, this is actually non-fatal, but we test it closes nicely)
#[tokio::test]
async fn test_text_before_session_init_returns_error() {
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

    // Should receive a no_session error
    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::Error {
            code,
            message,
            fatal,
            ..
        } => {
            assert_eq!(code, "no_session", "Error code should be no_session");
            assert!(!message.is_empty(), "Error message should not be empty");
            // The no_session error is non-fatal, allowing the client to recover
            assert!(!fatal, "no_session error should be non-fatal");
        }
        other => panic!("Expected Error, got: {other:?}"),
    }

    // Connection should still be open for recovery
    init_session(&mut ws, Some("mock"))
        .await
        .expect("Should be able to init session after error");

    server.shutdown();
}

/// Test: Fatal session.error when no providers are available
///
/// This is a fatal error that should close the connection.
#[tokio::test]
async fn test_fatal_error_no_providers_closes_connection() {
    // Spawn a server with no providers
    let server = TestServerBuilder::new().spawn().await;

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

    // Should receive session.error (fatal)
    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::SessionError { code, message, .. } => {
            assert_eq!(code, "no_providers", "Error code should be no_providers");
            assert!(
                message.contains("No TTS providers"),
                "Error message should explain no providers are available"
            );
        }
        other => panic!("Expected SessionError, got: {other:?}"),
    }

    // Connection should be closed after fatal error
    let next = timeout(Duration::from_millis(500), ws.next()).await;
    match next {
        Ok(Some(Ok(Message::Close(_)) | Err(_)) | None) | Err(_) => {
            // Expected - connection closed
        }
        Ok(Some(Ok(other))) => {
            panic!("Expected connection close after fatal error, got: {other:?}");
        }
    }

    server.shutdown();
}

/// Test: Fatal session.error when requesting non-existent provider
#[tokio::test]
async fn test_fatal_error_provider_not_found() {
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

    // Should receive session.error with alternatives
    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::SessionError {
            code,
            message,
            alternatives,
        } => {
            assert_eq!(
                code, "provider_unavailable",
                "Error code should be provider_unavailable"
            );
            assert!(
                message.contains("nonexistent_provider"),
                "Error message should mention the requested provider"
            );
            // Should include available alternatives
            let alts = alternatives.expect("Should have alternatives");
            assert!(
                alts.contains(&"mock".to_string()),
                "Alternatives should include the available mock provider"
            );
        }
        other => panic!("Expected SessionError, got: {other:?}"),
    }

    server.shutdown();
}

// ============================================================================
// Error Message Format Verification Tests
// ============================================================================

/// Test: Non-fatal error message includes all required fields
#[tokio::test]
async fn test_error_message_format_nonfatal() {
    let failing_provider =
        MockTtsProvider::new("error_format").with_synthesis_mode(MockSynthesisMode::FailInit {
            error: "Test error message".to_string(),
        });

    let server = TestServerBuilder::new()
        .with_provider(failing_provider)
        .with_default_provider("error_format")
        .spawn()
        .await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    init_session(&mut ws, Some("error_format"))
        .await
        .expect("Should initialize session");

    // Send text to trigger error
    // Note: SRX boundary detection requires following text to emit a sentence
    let text_msg = ClientMessage::Text {
        content: "Test. More text".to_string(),
    };
    send_message(&mut ws, &text_msg)
        .await
        .expect("Should send text");

    // Receive and verify error format
    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::Error {
            code,
            message,
            fatal,
            sentence_index,
        } => {
            // Verify all required fields are present and correct
            assert!(!code.is_empty(), "Error code should not be empty");
            assert!(!message.is_empty(), "Error message should not be empty");
            assert!(!fatal, "Non-fatal error should have fatal=false");
            assert!(
                sentence_index.is_some(),
                "Synthesis error should include sentence_index"
            );
        }
        other => panic!("Expected Error, got: {other:?}"),
    }

    server.shutdown();
}

/// Test: Fatal session.error message includes required fields
#[tokio::test]
async fn test_error_message_format_session_error() {
    let server = TestServerBuilder::new().spawn().await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Try to initialize session (will fail - no providers)
    let init_msg = ClientMessage::SessionInit {
        provider: None,
        voice: None,
        audio_format: None,
        code_block_mode: None,
    };
    send_message(&mut ws, &init_msg)
        .await
        .expect("Should send init message");

    // Receive and verify session.error format
    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::SessionError {
            code,
            message,
            alternatives,
        } => {
            // Verify required fields
            assert!(!code.is_empty(), "Error code should not be empty");
            assert!(!message.is_empty(), "Error message should not be empty");
            // alternatives may be None when there are no alternatives available
            assert!(
                alternatives.is_none() || alternatives.as_ref().is_some_and(Vec::is_empty),
                "No alternatives should be available when there are no providers"
            );
        }
        other => panic!("Expected SessionError, got: {other:?}"),
    }

    server.shutdown();
}

/// Test: Session.error with alternatives includes alternatives field
#[tokio::test]
async fn test_error_message_format_with_alternatives() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Request a non-existent provider (should return alternatives)
    let init_msg = ClientMessage::SessionInit {
        provider: Some("does_not_exist".to_string()),
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
            code,
            message,
            alternatives,
        } => {
            assert_eq!(code, "provider_unavailable");
            assert!(!message.is_empty(), "Error message should not be empty");
            let alts = alternatives.expect("Should have alternatives field");
            assert!(!alts.is_empty(), "Alternatives should not be empty");
            // All alternatives should be valid provider IDs (non-empty strings)
            for alt in &alts {
                assert!(
                    !alt.is_empty(),
                    "Alternative provider ID should not be empty"
                );
            }
        }
        other => panic!("Expected SessionError, got: {other:?}"),
    }

    server.shutdown();
}

/// Test: Invalid JSON message returns error with proper format
#[tokio::test]
async fn test_error_message_format_invalid_json() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Send invalid JSON
    ws.send(Message::Text("{ not valid json".into()))
        .await
        .expect("Should send message");

    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::Error {
            code,
            message,
            fatal,
            sentence_index,
        } => {
            assert_eq!(
                code, "invalid_message",
                "Error code should be invalid_message"
            );
            assert!(!message.is_empty(), "Error message should not be empty");
            assert!(!fatal, "Invalid message error should be non-fatal");
            assert!(
                sentence_index.is_none(),
                "Invalid message error should not have sentence_index"
            );
        }
        other => panic!("Expected Error, got: {other:?}"),
    }

    // Connection should still be open
    init_session(&mut ws, Some("mock"))
        .await
        .expect("Should be able to init session after invalid JSON");

    server.shutdown();
}

// ============================================================================
// Multiple Errors in Sequence Tests
// ============================================================================

/// Test: Multiple sentences with errors are all reported with correct indices
#[tokio::test]
#[allow(clippy::cast_possible_truncation)]
async fn test_multiple_sentences_with_errors_report_correct_indices() {
    // Provider fails for every sentence at initialization
    let failing_provider =
        MockTtsProvider::new("multi_fail").with_synthesis_mode(MockSynthesisMode::FailInit {
            error: "Synthesis failed".to_string(),
        });

    let server = TestServerBuilder::new()
        .with_provider(failing_provider)
        .with_default_provider("multi_fail")
        .spawn()
        .await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    init_session(&mut ws, Some("multi_fail"))
        .await
        .expect("Should initialize session");

    // Send text with multiple sentences
    // Note: SRX boundary detection requires following text to emit a sentence
    let text_msg = ClientMessage::Text {
        content: "First sentence. Second sentence. Third sentence. More text".to_string(),
    };
    send_message(&mut ws, &text_msg)
        .await
        .expect("Should send text");

    // Collect all error messages
    let mut errors = Vec::new();
    for _ in 0..10 {
        match timeout(Duration::from_millis(500), ws.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                let msg: ServerMessage = serde_json::from_str(&text).expect("Should parse message");
                if let ServerMessage::Error {
                    code,
                    sentence_index,
                    ..
                } = msg
                {
                    if code == "synthesis_failed" {
                        errors.push(sentence_index);
                    }
                }
            }
            _ => break,
        }
    }

    // Should have received 3 errors, one for each sentence
    assert_eq!(errors.len(), 3, "Should receive 3 errors for 3 sentences");

    // Each error should have a unique sentence_index
    for (i, idx) in errors.iter().enumerate() {
        assert_eq!(
            *idx,
            Some(i as u32),
            "Error {i} should have sentence_index {i}"
        );
    }

    // Session should still complete
    let result = finish_session(&mut ws).await;
    assert!(
        result.is_ok(),
        "Session should complete even with all errors"
    );

    server.shutdown();
}
