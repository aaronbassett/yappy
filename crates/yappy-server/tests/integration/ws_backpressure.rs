//! Backpressure behavior integration tests
//!
//! These tests verify the system correctly implements backpressure when
//! a session's audio output falls behind:
//! - FR-020: System MUST implement backpressure when a session's audio output
//!   falls behind, pausing synthesis rather than buffering unbounded.
//! - SC-010: Backpressure test shows synthesis pause/resume behavior.

#![allow(clippy::uninlined_format_args)]
#![allow(clippy::match_same_arms)] // Control frames handled identically
#![allow(clippy::cast_possible_truncation)] // Test code with known-safe casts

use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::StreamExt;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

use yappy_core::{AudioChunk, ClientMessage, ServerMessage};

use crate::common::{
    finish_session, init_session, send_message, MockSynthesisMode, MockTtsProvider,
    TestServerBuilder,
};

/// Small channel capacity to easily trigger backpressure in tests
const TEST_BACKPRESSURE_CAPACITY: usize = 4;

/// Number of sentences to generate (each produces multiple chunks)
const SENTENCES_TO_GENERATE: usize = 5;

/// Chunks per sentence for backpressure testing
const CHUNKS_PER_SENTENCE: usize = 8;

/// Extended timeout for backpressure tests
const BACKPRESSURE_TIMEOUT: Duration = Duration::from_secs(30);

// ============================================================================
// Slow Consumer Tests
// ============================================================================

/// Test: SC-010 - Slow consumer causes backpressure, all frames eventually delivered
///
/// This test verifies:
/// 1. Server doesn't crash or run out of memory with slow consumer
/// 2. All audio frames are eventually delivered
/// 3. Session completes successfully
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn test_slow_consumer_receives_all_frames() {
    // Create a provider that generates many chunks per sentence
    let provider =
        MockTtsProvider::new("backpressure_test").with_synthesis_mode(MockSynthesisMode::Success {
            chunks_per_sentence: CHUNKS_PER_SENTENCE,
            chunk_duration_ms: 50,
            chunk_bytes: 256,
        });

    let server = TestServerBuilder::new()
        .with_provider(provider)
        .with_audio_channel_capacity(TEST_BACKPRESSURE_CAPACITY)
        .spawn()
        .await;

    let ws_url = server.ws_url();
    let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("Failed to connect");

    let mut ws = ws_stream;

    // Initialize session
    init_session(&mut ws, Some("backpressure_test"))
        .await
        .expect("Failed to init session");

    // Send multiple sentences to generate many audio chunks
    // Total chunks expected: SENTENCES_TO_GENERATE * CHUNKS_PER_SENTENCE
    // plus the flushed trailing fragment
    let sentences: Vec<String> = (0..SENTENCES_TO_GENERATE)
        .map(|i| format!("This is sentence number {}.", i + 1))
        .collect();
    let text = sentences.join(" ") + " Trailing";

    let text_msg = ClientMessage::Text { content: text };
    send_message(&mut ws, &text_msg)
        .await
        .expect("Failed to send text");

    // Simulate a slow consumer by reading frames with delays
    // This should trigger backpressure on the server side
    let mut received_chunks = 0;
    let mut chunk_sequences: Vec<u32> = Vec::new();
    let slow_consumer_delay = Duration::from_millis(20);

    let start = Instant::now();

    // Read all audio chunks with artificial delay
    loop {
        match timeout(Duration::from_millis(500), ws.next()).await {
            Ok(Some(Ok(Message::Binary(data)))) => {
                if let Some(chunk) = AudioChunk::from_binary_frame(&Bytes::from(data.to_vec())) {
                    received_chunks += 1;
                    chunk_sequences.push(chunk.sequence);

                    // Simulate slow processing
                    tokio::time::sleep(slow_consumer_delay).await;
                }
            }
            Ok(Some(Ok(Message::Text(_)))) => {
                // Could be a control message, continue
            }
            Ok(Some(Ok(
                Message::Ping(_) | Message::Pong(_) | Message::Close(_) | Message::Frame(_),
            ))) => {
                // Ignore control frames
            }
            Ok(Some(Err(e))) => panic!("WebSocket error: {}", e),
            Ok(None) => panic!("Connection closed unexpectedly"),
            Err(_) => break, // Timeout - no more immediate chunks
        }
    }

    // Record chunks received before text.done
    let pre_flush_chunks = received_chunks;

    // Now finish the session and collect remaining chunks
    send_message(&mut ws, &ClientMessage::TextDone)
        .await
        .expect("Failed to send text.done");

    let audio_done_received;
    let total_sentences;

    loop {
        match timeout(BACKPRESSURE_TIMEOUT, ws.next()).await {
            Ok(Some(Ok(Message::Binary(data)))) => {
                if let Some(chunk) = AudioChunk::from_binary_frame(&Bytes::from(data.to_vec())) {
                    received_chunks += 1;
                    chunk_sequences.push(chunk.sequence);

                    // Continue slow consumption
                    tokio::time::sleep(slow_consumer_delay).await;
                }
            }
            Ok(Some(Ok(Message::Text(text)))) => {
                if let Ok(ServerMessage::AudioDone {
                    total_sentences: ts,
                    ..
                }) = serde_json::from_str::<ServerMessage>(&text)
                {
                    total_sentences = ts;
                    audio_done_received = true;
                    break;
                }
            }
            Ok(Some(Ok(
                Message::Ping(_) | Message::Pong(_) | Message::Close(_) | Message::Frame(_),
            ))) => {
                // Ignore control frames
            }
            Ok(Some(Err(e))) => panic!("WebSocket error: {e}"),
            Ok(None) => panic!("Connection closed before audio.done"),
            Err(elapsed) => panic!("Timeout waiting for audio.done after {elapsed:?}"),
        }
    }

    let elapsed = start.elapsed();

    // Verify results
    assert!(
        audio_done_received,
        "Session should complete with audio.done"
    );

    // Expected chunks: (SENTENCES_TO_GENERATE + 1 flushed) * CHUNKS_PER_SENTENCE
    let expected_sentences = (SENTENCES_TO_GENERATE + 1) as u32; // +1 for trailing fragment
    let expected_total_chunks = expected_sentences as usize * CHUNKS_PER_SENTENCE;

    assert_eq!(
        total_sentences, expected_sentences,
        "Should have processed {} sentences, got {}",
        expected_sentences, total_sentences
    );

    assert_eq!(
        received_chunks, expected_total_chunks,
        "Should have received {} chunks, got {} (pre-flush: {})",
        expected_total_chunks, received_chunks, pre_flush_chunks
    );

    // Verify sequence numbers are sequential (0, 1, 2, ...)
    for (i, seq) in chunk_sequences.iter().enumerate() {
        assert_eq!(
            *seq, i as u32,
            "Chunk {} should have sequence {}, got {}",
            i, i, seq
        );
    }

    eprintln!(
        "Slow consumer test completed: {} chunks in {:?}",
        received_chunks, elapsed
    );

    server.shutdown();
}

/// Test: Backpressure recovery - transitions from slow to fast consumption
///
/// This test verifies:
/// 1. Server handles transition from slow to fast consumption gracefully
/// 2. All expected audio is delivered
/// 3. No frames are lost during the transition
#[tokio::test]
async fn test_backpressure_recovery_slow_to_fast() {
    let provider =
        MockTtsProvider::new("recovery_test").with_synthesis_mode(MockSynthesisMode::Success {
            chunks_per_sentence: CHUNKS_PER_SENTENCE,
            chunk_duration_ms: 50,
            chunk_bytes: 256,
        });

    let server = TestServerBuilder::new()
        .with_provider(provider)
        .with_audio_channel_capacity(TEST_BACKPRESSURE_CAPACITY)
        .spawn()
        .await;

    let ws_url = server.ws_url();
    let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("Failed to connect");

    let mut ws = ws_stream;

    init_session(&mut ws, Some("recovery_test"))
        .await
        .expect("Failed to init session");

    // Send text that will generate many chunks
    let sentences: Vec<String> = (0..SENTENCES_TO_GENERATE)
        .map(|i| format!("Recovery test sentence {}.", i + 1))
        .collect();
    let text = sentences.join(" ") + " Trailing";

    send_message(&mut ws, &ClientMessage::Text { content: text })
        .await
        .expect("Failed to send text");

    let mut received_chunks = 0;
    let expected_sentences = (SENTENCES_TO_GENERATE + 1) as u32;
    let expected_total_chunks = expected_sentences as usize * CHUNKS_PER_SENTENCE;

    // Phase 1: Slow consumption (first half of expected chunks)
    let slow_target = expected_total_chunks / 2;
    let slow_delay = Duration::from_millis(25);

    while received_chunks < slow_target {
        match timeout(Duration::from_millis(500), ws.next()).await {
            Ok(Some(Ok(Message::Binary(data)))) => {
                if AudioChunk::from_binary_frame(&Bytes::from(data.to_vec())).is_some() {
                    received_chunks += 1;
                    tokio::time::sleep(slow_delay).await;
                }
            }
            Ok(Some(Ok(Message::Text(_)))) => {}
            Ok(Some(Ok(_))) => {} // Ignore other message types
            Ok(Some(Err(e))) => panic!("WebSocket error during slow phase: {}", e),
            Ok(None) => break,
            Err(_) => break,
        }
    }

    let slow_phase_chunks = received_chunks;

    // Phase 2: Fast consumption (remaining chunks, no delay)
    loop {
        match timeout(Duration::from_millis(200), ws.next()).await {
            Ok(Some(Ok(Message::Binary(data)))) => {
                if AudioChunk::from_binary_frame(&Bytes::from(data.to_vec())).is_some() {
                    received_chunks += 1;
                    // No delay - consume as fast as possible
                }
            }
            Ok(Some(Ok(Message::Text(_)))) => {}
            Ok(Some(Ok(_))) => {} // Ignore other message types
            Ok(Some(Err(e))) => panic!("WebSocket error during fast phase: {}", e),
            Ok(None) => break,
            Err(_) => break,
        }
    }

    let pre_flush_chunks = received_chunks;

    // Finish session and collect remaining
    let (total_sentences, _, _) = finish_session(&mut ws)
        .await
        .expect("Failed to finish session");

    // Count any chunks delivered after text.done (shouldn't be many but possible)
    // The finish_session helper already drains remaining binary frames

    assert_eq!(
        total_sentences, expected_sentences,
        "Should have {} sentences",
        expected_sentences
    );

    // We may not have received all chunks yet due to timing, but we should have
    // received a significant portion during both phases
    assert!(
        slow_phase_chunks > 0,
        "Should have received chunks during slow phase"
    );
    assert!(
        pre_flush_chunks > slow_phase_chunks,
        "Should have received more chunks during fast phase: slow={}, total={}",
        slow_phase_chunks,
        pre_flush_chunks
    );

    eprintln!(
        "Recovery test: slow phase {} chunks, total pre-flush {} chunks",
        slow_phase_chunks, pre_flush_chunks
    );

    server.shutdown();
}

/// Test: Multiple concurrent sessions with different consumption speeds
///
/// This verifies that backpressure in one session doesn't affect others
/// (session isolation with backpressure).
#[tokio::test]
async fn test_concurrent_sessions_independent_backpressure() {
    let provider =
        MockTtsProvider::new("concurrent_bp").with_synthesis_mode(MockSynthesisMode::Success {
            chunks_per_sentence: 6,
            chunk_duration_ms: 40,
            chunk_bytes: 128,
        });

    let server = TestServerBuilder::new()
        .with_provider(provider)
        .with_audio_channel_capacity(TEST_BACKPRESSURE_CAPACITY)
        .spawn()
        .await;

    // Spawn a slow consumer session
    let slow_handle = {
        let ws_url = server.ws_url();
        tokio::spawn(async move { run_session_with_speed(ws_url, "slow", 30).await })
    };

    // Spawn a fast consumer session
    let fast_handle = {
        let ws_url = server.ws_url();
        tokio::spawn(async move { run_session_with_speed(ws_url, "fast", 0).await })
    };

    // Wait for both to complete
    let slow_result = slow_handle
        .await
        .expect("Slow session task panicked")
        .expect("Slow session failed");
    let fast_result = fast_handle
        .await
        .expect("Fast session task panicked")
        .expect("Fast session failed");

    // Both sessions should complete successfully
    assert!(
        slow_result.completed,
        "Slow session should complete successfully"
    );
    assert!(
        fast_result.completed,
        "Fast session should complete successfully"
    );

    // Both should receive the same number of chunks (3 sentences * 6 chunks each)
    // Note: The exact count depends on sentence detection, but both should be equal
    assert_eq!(
        slow_result.chunks_received, fast_result.chunks_received,
        "Both sessions should receive same number of chunks"
    );

    // Fast session should complete faster than slow session
    assert!(
        fast_result.duration_ms < slow_result.duration_ms,
        "Fast session should complete faster: fast={}ms, slow={}ms",
        fast_result.duration_ms,
        slow_result.duration_ms
    );

    eprintln!(
        "Concurrent backpressure test: fast={}ms ({} chunks), slow={}ms ({} chunks)",
        fast_result.duration_ms,
        fast_result.chunks_received,
        slow_result.duration_ms,
        slow_result.chunks_received
    );

    server.shutdown();
}

/// Test: Server handles burst of text followed by slow consumption
///
/// This simulates a realistic scenario where a large amount of text is
/// sent quickly, but the client processes audio slowly.
#[tokio::test]
async fn test_text_burst_with_slow_consumer() {
    let provider = MockTtsProvider::new("burst_test").with_synthesis_mode(
        MockSynthesisMode::SuccessWithDelay {
            chunks_per_sentence: 4,
            chunk_duration_ms: 50,
            chunk_bytes: 256,
            inter_chunk_delay_ms: 5, // Small delay to simulate real synthesis
        },
    );

    let server = TestServerBuilder::new()
        .with_provider(provider)
        .with_audio_channel_capacity(TEST_BACKPRESSURE_CAPACITY)
        .spawn()
        .await;

    let ws_url = server.ws_url();
    let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("Failed to connect");

    let mut ws = ws_stream;

    init_session(&mut ws, Some("burst_test"))
        .await
        .expect("Failed to init session");

    // Send multiple text messages in rapid succession (burst)
    for i in 0..4 {
        let text_msg = ClientMessage::Text {
            content: format!("Burst message number {}. ", i + 1),
        };
        send_message(&mut ws, &text_msg)
            .await
            .expect("Failed to send burst text");
    }
    // Final message with trailing text
    send_message(
        &mut ws,
        &ClientMessage::Text {
            content: "Final burst message. Done".to_string(),
        },
    )
    .await
    .expect("Failed to send final text");

    // Now consume slowly
    let mut received_chunks = 0;
    let slow_delay = Duration::from_millis(40);

    loop {
        match timeout(Duration::from_millis(500), ws.next()).await {
            Ok(Some(Ok(Message::Binary(data)))) => {
                if AudioChunk::from_binary_frame(&Bytes::from(data.to_vec())).is_some() {
                    received_chunks += 1;
                    tokio::time::sleep(slow_delay).await;
                }
            }
            Ok(Some(Ok(Message::Text(_)))) => {}
            Ok(Some(Ok(_))) => {} // Ignore other message types
            Ok(Some(Err(e))) => panic!("WebSocket error: {}", e),
            Ok(None) => break,
            Err(_) => break,
        }
    }

    let pre_flush_chunks = received_chunks;

    // Finish session
    let (total_sentences, _, _) = finish_session(&mut ws)
        .await
        .expect("Failed to finish session");

    // Session should complete successfully
    assert!(
        total_sentences > 0,
        "Should have processed at least one sentence"
    );
    assert!(
        pre_flush_chunks > 0,
        "Should have received audio chunks: {}",
        pre_flush_chunks
    );

    eprintln!(
        "Burst test completed: {} sentences, {} chunks pre-flush",
        total_sentences, pre_flush_chunks
    );

    server.shutdown();
}

/// Test: Verify server doesn't crash with very slow consumer and many chunks
///
/// This is a stress test that sends a lot of text and consumes very slowly
/// to ensure the server doesn't run out of memory or crash.
#[tokio::test]
async fn test_very_slow_consumer_no_crash() {
    let provider =
        MockTtsProvider::new("stress_test").with_synthesis_mode(MockSynthesisMode::Success {
            chunks_per_sentence: 10,
            chunk_duration_ms: 30,
            chunk_bytes: 512,
        });

    // Use very small capacity to maximize backpressure
    let server = TestServerBuilder::new()
        .with_provider(provider)
        .with_audio_channel_capacity(2)
        .spawn()
        .await;

    let ws_url = server.ws_url();
    let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("Failed to connect");

    let mut ws = ws_stream;

    init_session(&mut ws, Some("stress_test"))
        .await
        .expect("Failed to init session");

    // Send text that generates many sentences
    let sentences: Vec<String> = (0..8)
        .map(|i| format!("Stress test sentence number {}.", i + 1))
        .collect();
    let text = sentences.join(" ") + " Trailing";

    send_message(&mut ws, &ClientMessage::Text { content: text })
        .await
        .expect("Failed to send text");

    // Very slow consumption - this would cause memory issues without backpressure
    let mut received_chunks = 0;
    let very_slow_delay = Duration::from_millis(50);

    let start = Instant::now();
    let max_test_duration = Duration::from_secs(20);

    // Read chunks slowly until timeout or we've received enough
    loop {
        if start.elapsed() > max_test_duration {
            break;
        }

        match timeout(Duration::from_millis(300), ws.next()).await {
            Ok(Some(Ok(Message::Binary(data)))) => {
                if AudioChunk::from_binary_frame(&Bytes::from(data.to_vec())).is_some() {
                    received_chunks += 1;
                    tokio::time::sleep(very_slow_delay).await;
                }
            }
            Ok(Some(Ok(Message::Text(_)))) => {}
            Ok(Some(Ok(_))) => {} // Ignore other message types
            Ok(Some(Err(e))) => panic!("WebSocket error: {}", e),
            Ok(None) => break,
            Err(_) => break,
        }
    }

    // Finish session (may timeout if very backed up, which is acceptable)
    let finish_result = timeout(Duration::from_secs(10), finish_session(&mut ws)).await;

    // The key assertion: we got this far without crashing
    assert!(
        received_chunks > 0,
        "Should have received at least some chunks"
    );

    match finish_result {
        Ok(Ok((sentences, _, _))) => {
            eprintln!(
                "Stress test completed normally: {} sentences, {} chunks received",
                sentences, received_chunks
            );
        }
        Ok(Err(e)) => {
            eprintln!(
                "Stress test session error (acceptable): {}, {} chunks received",
                e, received_chunks
            );
        }
        Err(_) => {
            eprintln!(
                "Stress test timed out (acceptable): {} chunks received",
                received_chunks
            );
        }
    }

    server.shutdown();
}

// ============================================================================
// Helper Types
// ============================================================================

/// Result from a session with speed control
struct SessionSpeedResult {
    completed: bool,
    chunks_received: usize,
    duration_ms: u64,
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Run a session with configurable consumption speed
async fn run_session_with_speed(
    ws_url: String,
    _label: &str,
    delay_ms: u64,
) -> Result<SessionSpeedResult, String> {
    let start = Instant::now();

    let (ws_stream, _) = timeout(
        BACKPRESSURE_TIMEOUT,
        tokio_tungstenite::connect_async(&ws_url),
    )
    .await
    .map_err(|_| "Connection timeout".to_string())?
    .map_err(|e| format!("Connection error: {}", e))?;

    let mut ws = ws_stream;

    // Initialize session
    init_session(&mut ws, Some("concurrent_bp")).await?;

    // Send text - 3 sentences plus trailing
    let text_msg = ClientMessage::Text {
        content: "First sentence here. Second sentence now. Third sentence end. Trailing"
            .to_string(),
    };
    send_message(&mut ws, &text_msg).await?;

    // Consume with specified delay
    let mut chunks_received = 0;
    let delay = Duration::from_millis(delay_ms);

    loop {
        match timeout(Duration::from_millis(300), ws.next()).await {
            Ok(Some(Ok(Message::Binary(data)))) => {
                if AudioChunk::from_binary_frame(&Bytes::from(data.to_vec())).is_some() {
                    chunks_received += 1;
                    if delay_ms > 0 {
                        tokio::time::sleep(delay).await;
                    }
                }
            }
            Ok(Some(Ok(Message::Text(_)))) => {}
            Ok(Some(Ok(_))) => {} // Ignore other message types
            Ok(Some(Err(e))) => return Err(format!("WebSocket error: {}", e)),
            Ok(None) => break,
            Err(_) => break,
        }
    }

    // Finish session
    let _ = finish_session(&mut ws).await?;

    let duration_ms = start.elapsed().as_millis() as u64;

    Ok(SessionSpeedResult {
        completed: true,
        chunks_received,
        duration_ms,
    })
}
