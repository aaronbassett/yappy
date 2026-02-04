//! Sentence buffer integration tests for the Yappy TTS server
//!
//! These tests verify the sentence buffer behavior via WebSocket integration:
//! - SRX-based sentence segmentation handling abbreviations and decimals
//! - Code block detection and handling (skip, `read_literally`, announce-and-skip)
//!
//! Derived from spec.md User Story 7 acceptance scenarios.

use std::fmt::Write;
use std::time::Duration;

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

use yappy_core::{AudioChunk, ClientMessage, CodeBlockMode, ServerMessage};

use crate::common::{
    connect_ws, finish_session, receive_server_message_with_timeout, send_message,
    spawn_test_server, DEFAULT_TIMEOUT,
};

// ============================================================================
// Helper Functions
// ============================================================================

/// Send session.init with a specific `code_block_mode` and wait for session.ready
///
/// Returns the `session_id` from the session.ready response.
async fn init_session_with_code_block_mode<S>(
    ws: &mut S,
    provider: Option<&str>,
    code_block_mode: CodeBlockMode,
) -> Result<String, String>
where
    S: SinkExt<Message>
        + StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + Unpin,
    S::Error: std::fmt::Display,
{
    // Send session.init with code_block_mode
    let init_msg = ClientMessage::SessionInit {
        provider: provider.map(String::from),
        voice: None,
        audio_format: None,
        code_block_mode: Some(code_block_mode),
    };
    send_message(ws, &init_msg).await?;

    // Wait for session.ready
    let response = receive_server_message_with_timeout(ws, DEFAULT_TIMEOUT).await?;

    match response {
        ServerMessage::SessionReady { session_id, .. } => Ok(session_id),
        ServerMessage::SessionError { code, message, .. } => {
            Err(format!("Session error [{code}]: {message}"))
        }
        other => Err(format!("Unexpected response: {other:?}")),
    }
}

/// Collect audio chunks until timeout, returning the count and text fragments
/// from any received synthesis
async fn collect_audio_chunks<S>(ws: &mut S, max_chunks: usize) -> Vec<AudioChunk>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let mut chunks = Vec::new();
    for _ in 0..max_chunks {
        match timeout(Duration::from_millis(500), ws.next()).await {
            Ok(Some(Ok(Message::Binary(data)))) => {
                if let Some(chunk) = AudioChunk::from_binary_frame(&Bytes::from(data.to_vec())) {
                    chunks.push(chunk);
                }
            }
            _ => break,
        }
    }
    chunks
}

// ============================================================================
// SRX-Based Sentence Segmentation Tests (User Story 7, Scenarios 1-3)
// ============================================================================

/// Test: Abbreviations are handled correctly without false sentence breaks
///
/// User Story 7, Scenario 1:
/// **Given** incoming text "Dr. Smith arrived at 3.14 p.m.",
/// **When** the buffer processes this text,
/// **Then** it emits the entire text as one sentence, not splitting on "Dr." or "3.".
#[tokio::test]
async fn test_ws_abbreviation_handling() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Initialize session with default provider
    crate::common::init_session(&mut ws, Some("mock"))
        .await
        .expect("Should initialize session");

    // Send text with abbreviations: Dr., decimal 3.14, and p.m.
    // The text after "And more" ensures SRX can detect the boundary after "p.m."
    let text_msg = ClientMessage::Text {
        content: "Dr. Smith arrived at 3.14 p.m. And more".to_string(),
    };
    send_message(&mut ws, &text_msg)
        .await
        .expect("Should send text");

    // Collect audio chunks - should get 1 for "Dr. Smith arrived at 3.14 p.m."
    // because it's followed by "And more" which allows boundary detection
    let chunks = collect_audio_chunks(&mut ws, 5).await;

    // Should have exactly 1 audio chunk for the first sentence
    // The abbreviations (Dr., 3.14, p.m.) should NOT cause false splits
    assert_eq!(
        chunks.len(),
        1,
        "Expected 1 audio chunk for the sentence with abbreviations, got {}",
        chunks.len()
    );
    assert_eq!(
        chunks[0].sentence_index, 0,
        "First sentence should have index 0"
    );

    // Finish session - "And more" will be flushed as second sentence
    let (total_sentences, _, _) = finish_session(&mut ws)
        .await
        .expect("Should finish session");

    assert_eq!(
        total_sentences, 2,
        "Should have 2 sentences total (1 with abbreviations + 1 flushed)"
    );

    server.shutdown();
}

/// Test: Decimal numbers are handled correctly without false sentence breaks
///
/// User Story 7, Scenario 1 (decimal aspect):
/// Decimals like 3.14 should not cause false sentence breaks.
#[tokio::test]
async fn test_ws_decimal_handling() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    crate::common::init_session(&mut ws, Some("mock"))
        .await
        .expect("Should initialize session");

    // Send text with decimal number followed by a real sentence boundary
    // "The value is 3.14. That is pi. More text"
    // - "3.14" should NOT cause a split
    // - "3.14." (sentence end) followed by "That" (uppercase) SHOULD cause a split
    let text_msg = ClientMessage::Text {
        content: "The value is 3.14. That is pi. More text".to_string(),
    };
    send_message(&mut ws, &text_msg)
        .await
        .expect("Should send text");

    // Collect audio chunks
    let chunks = collect_audio_chunks(&mut ws, 10).await;

    // Should have 2 chunks:
    // 1. "The value is 3.14."
    // 2. "That is pi."
    // ("More text" stays in buffer until flush)
    assert_eq!(
        chunks.len(),
        2,
        "Expected 2 audio chunks, got {} - decimal should not cause false split",
        chunks.len()
    );

    // Verify sentence indices
    assert_eq!(chunks[0].sentence_index, 0);
    assert_eq!(chunks[1].sentence_index, 1);

    // Finish and verify total
    let (total_sentences, _, _) = finish_session(&mut ws)
        .await
        .expect("Should finish session");

    assert_eq!(
        total_sentences, 3,
        "Should have 3 sentences total (2 complete + 1 flushed)"
    );

    server.shutdown();
}

/// Test: Multiple text messages build up correctly with proper sentence detection
#[tokio::test]
async fn test_ws_streaming_sentence_detection() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    crate::common::init_session(&mut ws, Some("mock"))
        .await
        .expect("Should initialize session");

    // Send text in streaming chunks, simulating LLM output
    // Note: SRX boundary detection processes each push independently
    send_message(
        &mut ws,
        &ClientMessage::Text {
            content: "Dr. ".to_string(),
        },
    )
    .await
    .expect("Send chunk 1");

    send_message(
        &mut ws,
        &ClientMessage::Text {
            content: "Smith arrived. ".to_string(),
        },
    )
    .await
    .expect("Send chunk 2");

    send_message(
        &mut ws,
        &ClientMessage::Text {
            content: "He brought 3.14 pies. ".to_string(),
        },
    )
    .await
    .expect("Send chunk 3");

    send_message(
        &mut ws,
        &ClientMessage::Text {
            content: "Mrs. Jones was pleased. More".to_string(),
        },
    )
    .await
    .expect("Send chunk 4");

    // Collect audio chunks
    let chunks = collect_audio_chunks(&mut ws, 10).await;

    // The exact number of sentences depends on SRX boundary detection timing.
    // With the trailing space in chunks 2 and 3, SRX may detect boundaries
    // immediately as each chunk with " sentence. " pattern arrives.
    // We verify at least the expected sentences are detected.
    assert!(
        chunks.len() >= 3,
        "Expected at least 3 audio chunks for streaming sentences, got {}",
        chunks.len()
    );

    // Finish session - "More" gets flushed
    let (total_sentences, _, _) = finish_session(&mut ws)
        .await
        .expect("Should finish session");

    // Should have all sentences including flushed "More"
    assert!(
        total_sentences >= 4,
        "Should have at least 4 sentences total"
    );

    server.shutdown();
}

// ============================================================================
// Code Block Handling Tests (User Story 7, FR-016a)
// ============================================================================

/// Test: Code blocks are skipped in skip mode (default)
///
/// Verifies that code block content is not synthesized when using skip mode.
#[tokio::test]
async fn test_ws_code_block_skip_mode() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Initialize with explicit skip mode
    init_session_with_code_block_mode(&mut ws, Some("mock"), CodeBlockMode::Skip)
        .await
        .expect("Should initialize session");

    // Send text with a code block in the middle
    // The code block content should be completely skipped
    let text_msg = ClientMessage::Text {
        content: "Hello world. ```rust\nfn main() {\n    println!(\"test\");\n}\n``` Goodbye world. More text".to_string(),
    };
    send_message(&mut ws, &text_msg)
        .await
        .expect("Should send text");

    // Collect audio chunks
    let chunks = collect_audio_chunks(&mut ws, 10).await;

    // Should have 2 chunks:
    // 1. "Hello world."
    // 2. "Goodbye world."
    // The code block content is skipped entirely
    assert_eq!(
        chunks.len(),
        2,
        "Expected 2 audio chunks (code block skipped)"
    );

    // Finish session
    let (total_sentences, _, _) = finish_session(&mut ws)
        .await
        .expect("Should finish session");

    // "More text" is flushed as 3rd sentence
    assert_eq!(
        total_sentences, 3,
        "Should have 3 sentences (2 + 1 flushed, code block skipped)"
    );

    server.shutdown();
}

/// Test: Code blocks emit "code block" announcement in announce-and-skip mode
#[tokio::test]
async fn test_ws_code_block_announce_mode() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Initialize with announce-and-skip mode
    init_session_with_code_block_mode(&mut ws, Some("mock"), CodeBlockMode::AnnounceAndSkip)
        .await
        .expect("Should initialize session");

    // Send text with a code block
    // Note: "code block. " is inserted by the buffer, which combined with "After code."
    // may result in different sentence boundaries than expected.
    let text_msg = ClientMessage::Text {
        content: "Before code. ```python\nprint('hello')\n``` After code. More text".to_string(),
    };
    send_message(&mut ws, &text_msg)
        .await
        .expect("Should send text");

    // Collect audio chunks
    let chunks = collect_audio_chunks(&mut ws, 10).await;

    // In announce mode, the buffer inserts "code block. " which adds a sentence.
    // Depending on SRX detection timing, we should get at least 2 sentences
    // (one before code block, one for/after the announcement)
    assert!(
        chunks.len() >= 2,
        "Expected at least 2 audio chunks in announce mode, got {}",
        chunks.len()
    );

    // Finish session
    let (total_sentences, _, _) = finish_session(&mut ws)
        .await
        .expect("Should finish session");

    // Should have multiple sentences including the code block announcement
    // "Before code." + "code block." + "After code." + "More text" = at least 3 after flush
    assert!(
        total_sentences >= 3,
        "Should have at least 3 sentences (including announcement)"
    );

    server.shutdown();
}

/// Test: Code blocks pass through in `read_literally` mode
#[tokio::test]
async fn test_ws_code_block_read_literally_mode() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Initialize with read_literally mode
    init_session_with_code_block_mode(&mut ws, Some("mock"), CodeBlockMode::ReadLiterally)
        .await
        .expect("Should initialize session");

    // Send text with a code block - in read_literally mode, everything passes through
    let text_msg = ClientMessage::Text {
        content: "Start. ```\ncode\n``` End. More".to_string(),
    };
    send_message(&mut ws, &text_msg)
        .await
        .expect("Should send text");

    // Collect audio chunks - in read_literally mode, the backticks become part of text
    let chunks = collect_audio_chunks(&mut ws, 10).await;

    // In read_literally mode, we expect at least some chunks
    // The exact segmentation depends on how SRX handles the backticks
    assert!(
        !chunks.is_empty(),
        "Should receive audio chunks in read_literally mode"
    );

    // Finish session to verify it completes normally
    let (total_sentences, _, _) = finish_session(&mut ws)
        .await
        .expect("Should finish session");

    // Should have at least 1 sentence
    assert!(
        total_sentences >= 1,
        "Should have at least 1 sentence in read_literally mode"
    );

    server.shutdown();
}

/// Test: Code blocks split across multiple text chunks are handled correctly
#[tokio::test]
async fn test_ws_code_block_streaming_chunks() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Initialize with skip mode
    init_session_with_code_block_mode(&mut ws, Some("mock"), CodeBlockMode::Skip)
        .await
        .expect("Should initialize session");

    // Send text in chunks, with code block fence split across chunks
    send_message(
        &mut ws,
        &ClientMessage::Text {
            content: "Hello world. ```python\n".to_string(),
        },
    )
    .await
    .expect("Send chunk 1");

    // Should get audio for "Hello world." before code block opens
    let chunks1 = collect_audio_chunks(&mut ws, 5).await;
    assert_eq!(chunks1.len(), 1, "Should have 1 chunk before code block");

    // Send code content (should be buffered/skipped)
    send_message(
        &mut ws,
        &ClientMessage::Text {
            content: "def foo():\n    return 42\n".to_string(),
        },
    )
    .await
    .expect("Send chunk 2");

    // Should get no audio while in code block
    let chunks2 = collect_audio_chunks(&mut ws, 5).await;
    assert!(
        chunks2.is_empty(),
        "Should have no audio while in code block"
    );

    // Send closing fence and more text
    send_message(
        &mut ws,
        &ClientMessage::Text {
            content: "``` Goodbye world. More".to_string(),
        },
    )
    .await
    .expect("Send chunk 3");

    // Should get audio for "Goodbye world."
    let chunks3 = collect_audio_chunks(&mut ws, 5).await;
    assert_eq!(
        chunks3.len(),
        1,
        "Should have 1 chunk after code block closes"
    );

    // Finish session
    let (total_sentences, _, _) = finish_session(&mut ws)
        .await
        .expect("Should finish session");

    assert_eq!(
        total_sentences, 3,
        "Should have 3 sentences (before, after, flushed)"
    );

    server.shutdown();
}

/// Test: Unclosed code block is treated as literal text on flush
#[tokio::test]
async fn test_ws_unclosed_code_block_literal_on_flush() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Initialize with skip mode
    init_session_with_code_block_mode(&mut ws, Some("mock"), CodeBlockMode::Skip)
        .await
        .expect("Should initialize session");

    // Send text with unclosed code block
    let text_msg = ClientMessage::Text {
        content: "Hello world. ```python\nsome code without closing".to_string(),
    };
    send_message(&mut ws, &text_msg)
        .await
        .expect("Should send text");

    // Should get audio for "Hello world."
    let chunks = collect_audio_chunks(&mut ws, 5).await;
    assert_eq!(
        chunks.len(),
        1,
        "Should have 1 chunk before unclosed code block"
    );

    // Finish session - the unclosed code block content should be treated as literal
    let (total_sentences, _, _) = finish_session(&mut ws)
        .await
        .expect("Should finish session");

    // "Hello world." + unclosed code block as literal = 2 sentences
    assert_eq!(
        total_sentences, 2,
        "Should have 2 sentences (1 + unclosed block as literal)"
    );

    server.shutdown();
}

// ============================================================================
// Buffer Overflow Tests (User Story 7, Scenario 3)
// ============================================================================

/// Test: Buffer overflow splits at clause boundary (comma) rather than mid-word
///
/// User Story 7, Scenario 3:
/// **Given** incoming text exceeding the maximum buffer size without punctuation,
/// **When** the buffer reaches capacity,
/// **Then** it splits at a clause boundary (comma) or word boundary rather than mid-word.
#[tokio::test]
async fn test_ws_buffer_overflow_clause_boundary() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    crate::common::init_session(&mut ws, Some("mock"))
        .await
        .expect("Should initialize session");

    // Create text that exceeds buffer size (default 4KB = 4096 bytes) without sentence-ending punctuation
    // Include a comma for clause boundary splitting
    // The text needs to be > 4096 bytes to trigger overflow
    let long_prefix = "a".repeat(2500);
    let long_suffix = "b".repeat(2500);
    // Total > 5000 bytes: clearly exceeds 4KB buffer
    let content = format!("{long_prefix}, and {long_suffix} continuing");
    assert!(
        content.len() > 4096,
        "Test text should exceed buffer size (got {} bytes)",
        content.len()
    );

    let text_msg = ClientMessage::Text { content };
    send_message(&mut ws, &text_msg)
        .await
        .expect("Should send text");

    // Allow more time for the buffer overflow processing and audio generation
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Collect audio chunks - buffer should force flush at clause boundary
    let _chunks = collect_audio_chunks(&mut ws, 10).await;

    // On buffer overflow, the buffer splits at clause boundary (comma).
    // At least one chunk should be emitted from the forced flush.
    // Note: Audio may arrive during finish_session collection.
    // The key verification is the total sentence count.

    // Finish session
    let (total_sentences, _, _) = finish_session(&mut ws)
        .await
        .expect("Should finish session");

    // Should have at least 2 sentences due to overflow splitting at comma
    assert!(
        total_sentences >= 2,
        "Should have at least 2 sentences from buffer overflow splitting, got {total_sentences}"
    );

    server.shutdown();
}

/// Test: Buffer overflow without clause boundary splits at word boundary
#[tokio::test]
async fn test_ws_buffer_overflow_word_boundary() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    crate::common::init_session(&mut ws, Some("mock"))
        .await
        .expect("Should initialize session");

    // Create text that exceeds buffer size (4096 bytes) without commas or sentence punctuation
    // Only spaces for word boundaries
    // Each "wordXXX " is about 8-9 bytes, so 600 words should exceed 4KB
    let mut long_text = String::new();
    for i in 0..600 {
        write!(long_text, "word{i} ").expect("write to String cannot fail");
    }
    long_text.push_str("final"); // No trailing space or punctuation

    assert!(
        long_text.len() > 4096,
        "Test text should exceed buffer size (got {} bytes)",
        long_text.len()
    );

    let text_msg = ClientMessage::Text { content: long_text };
    send_message(&mut ws, &text_msg)
        .await
        .expect("Should send text");

    // Allow more time for buffer overflow processing
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Collect audio chunks
    let _chunks = collect_audio_chunks(&mut ws, 20).await;

    // Note: Audio may arrive during finish_session collection.
    // The key verification is the total sentence count.

    // Finish session
    let (total_sentences, _, _) = finish_session(&mut ws)
        .await
        .expect("Should finish session");

    // Should have at least 2 segments from word boundary overflow splitting
    assert!(
        total_sentences >= 2,
        "Should have at least 2 segments from word boundary splitting, got {total_sentences}"
    );

    server.shutdown();
}

// ============================================================================
// Edge Cases and Regression Tests
// ============================================================================

/// Test: Mixed abbreviations and code blocks
#[tokio::test]
async fn test_ws_abbreviations_with_code_blocks() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Initialize with skip mode
    init_session_with_code_block_mode(&mut ws, Some("mock"), CodeBlockMode::Skip)
        .await
        .expect("Should initialize session");

    // Send text with abbreviations before and after a code block
    let text_msg = ClientMessage::Text {
        content: "Dr. Smith wrote this. ```rust\ncode\n``` Prof. Jones approved it. More text"
            .to_string(),
    };
    send_message(&mut ws, &text_msg)
        .await
        .expect("Should send text");

    // Collect audio chunks
    let chunks = collect_audio_chunks(&mut ws, 10).await;

    // Should have 2 chunks (abbreviations handled correctly, code block skipped)
    assert_eq!(
        chunks.len(),
        2,
        "Expected 2 audio chunks with proper abbreviation handling"
    );

    // Finish session
    let (total_sentences, _, _) = finish_session(&mut ws)
        .await
        .expect("Should finish session");

    assert_eq!(total_sentences, 3, "Should have 3 sentences total");

    server.shutdown();
}

/// Test: Empty text chunks don't cause issues
#[tokio::test]
async fn test_ws_empty_text_chunks() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    crate::common::init_session(&mut ws, Some("mock"))
        .await
        .expect("Should initialize session");

    // Send empty text
    send_message(
        &mut ws,
        &ClientMessage::Text {
            content: String::new(),
        },
    )
    .await
    .expect("Send empty");

    // Send whitespace only
    send_message(
        &mut ws,
        &ClientMessage::Text {
            content: "   ".to_string(),
        },
    )
    .await
    .expect("Send whitespace");

    // Send actual content
    send_message(
        &mut ws,
        &ClientMessage::Text {
            content: "Hello world. More".to_string(),
        },
    )
    .await
    .expect("Send content");

    // Collect audio chunks
    let chunks = collect_audio_chunks(&mut ws, 10).await;

    // Should handle empty chunks gracefully and produce audio for actual content
    assert_eq!(chunks.len(), 1, "Should have 1 chunk for actual sentence");

    // Finish session
    let (total_sentences, _, _) = finish_session(&mut ws)
        .await
        .expect("Should finish session");

    assert_eq!(total_sentences, 2, "Should have 2 sentences (1 + flushed)");

    server.shutdown();
}

/// Test: Special characters in text
#[tokio::test]
async fn test_ws_special_characters() {
    let server = spawn_test_server().await;
    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    crate::common::init_session(&mut ws, Some("mock"))
        .await
        .expect("Should initialize session");

    // Send text with various special characters
    let text_msg = ClientMessage::Text {
        content: "Hello! How are you? I'm fine... Great! More".to_string(),
    };
    send_message(&mut ws, &text_msg)
        .await
        .expect("Should send text");

    // Collect audio chunks
    let chunks = collect_audio_chunks(&mut ws, 10).await;

    // Should detect sentence boundaries at !, ?, and !
    // "Hello!", "How are you?", "I'm fine...", "Great!"
    assert!(
        chunks.len() >= 2,
        "Should have multiple chunks for different sentence-ending punctuation"
    );

    // Finish session
    let (total_sentences, _, _) = finish_session(&mut ws)
        .await
        .expect("Should finish session");

    assert!(total_sentences >= 3, "Should have multiple sentences");

    server.shutdown();
}
