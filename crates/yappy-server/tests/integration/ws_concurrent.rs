//! Concurrent WebSocket sessions integration tests
//!
//! These tests verify the system handles multiple concurrent WebSocket sessions correctly:
//! - FR-017: System MUST handle multiple concurrent WebSocket sessions
//! - FR-018: Each session MUST be fully isolated - no cross-contamination
//! - SC-009: 10 concurrent sessions complete independently without cross-contamination

#![allow(clippy::uninlined_format_args)]

use std::collections::HashSet;
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

use yappy_core::{AudioChunk, ClientMessage, ServerMessage};

use crate::common::{
    connect_ws, finish_session, init_session, send_message, MockSynthesisMode, MockTtsProvider,
    TestServerBuilder, DEFAULT_TIMEOUT,
};

/// Number of concurrent sessions to test (SC-009 specifies 10)
const CONCURRENT_SESSIONS: usize = 10;

/// Extended timeout for concurrent operations
const CONCURRENT_TIMEOUT: Duration = Duration::from_secs(30);

// ============================================================================
// Basic Concurrent Sessions Tests
// ============================================================================

/// Test: SC-009 - 10 concurrent sessions complete independently without cross-contamination
///
/// This test verifies:
/// 1. All sessions can connect and initialize successfully
/// 2. Each session receives a unique session ID
/// 3. All sessions complete their TTS flow correctly
/// 4. No session interferes with another
#[tokio::test]
async fn test_concurrent_sessions_complete_independently() {
    let server = TestServerBuilder::new()
        .with_mock_provider("mock")
        .spawn()
        .await;

    // Spawn all sessions concurrently
    let handles: Vec<_> = (0..CONCURRENT_SESSIONS)
        .map(|i| {
            let ws_url = server.ws_url();
            tokio::spawn(async move { run_session_with_unique_text(ws_url, i).await })
        })
        .collect();

    // Collect all results
    let results: Vec<SessionResult> = futures_util::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.expect("Task should not panic"))
        .collect::<Result<Vec<_>, _>>()
        .expect("All sessions should complete successfully");

    // Verify all session IDs are unique
    let session_ids: HashSet<_> = results.iter().map(|r| r.session_id.clone()).collect();
    assert_eq!(
        session_ids.len(),
        CONCURRENT_SESSIONS,
        "All {} sessions should have unique session IDs",
        CONCURRENT_SESSIONS
    );

    // Verify all sessions have the ses_ prefix
    for result in &results {
        assert!(
            result.session_id.starts_with("ses_"),
            "Session ID '{}' should start with 'ses_'",
            result.session_id
        );
    }

    // Verify all sessions received audio and completed
    for (i, result) in results.iter().enumerate() {
        assert!(
            result.audio_chunks_received > 0,
            "Session {} should have received audio chunks",
            i
        );
        assert!(
            result.audio_done_received,
            "Session {} should have received audio.done",
            i
        );
    }

    server.shutdown();
}

/// Test: Session isolation - each session receives only its own audio
///
/// This test sends different text to each session and verifies that:
/// 1. Each session receives the expected number of sentences based on its input
/// 2. Audio chunks have correct sequence numbers starting from 0
#[tokio::test]
#[allow(clippy::cast_possible_truncation)]
async fn test_session_isolation_no_cross_contamination() {
    let server = TestServerBuilder::new()
        .with_mock_provider("mock")
        .spawn()
        .await;

    // Spawn sessions with different sentence counts
    let handles: Vec<_> = (0..5)
        .map(|i| {
            let ws_url = server.ws_url();
            let sentence_count = i + 1; // 1, 2, 3, 4, 5 sentences
            tokio::spawn(
                async move { run_session_with_sentence_count(ws_url, i, sentence_count).await },
            )
        })
        .collect();

    // Collect all results
    let results: Vec<SessionIsolationResult> = futures_util::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.expect("Task should not panic"))
        .collect::<Result<Vec<_>, _>>()
        .expect("All sessions should complete successfully");

    // Verify each session received the correct number of sentences
    // Note: Each session sends (i+1) complete sentences plus a trailing fragment
    // that gets flushed on text.done, so total is (i+1) + 1 = (i+2)
    for (i, result) in results.iter().enumerate() {
        let expected_sentences = (i + 1) + 1; // sentences + flushed trailing fragment
        assert_eq!(
            result.total_sentences, expected_sentences as u32,
            "Session {} should have received {} sentences, got {}",
            i, expected_sentences, result.total_sentences
        );

        // Verify sequence numbers are sequential starting from 0
        let sequences = result.chunk_sequences.clone();
        for (idx, seq) in sequences.iter().enumerate() {
            assert_eq!(
                *seq, idx as u32,
                "Session {} chunk {} should have sequence {}, got {}",
                i, idx, idx, seq
            );
        }
    }

    server.shutdown();
}

/// Test: Concurrent sessions with different configurations
///
/// This test verifies sessions can use different providers/voices
/// without interference
#[tokio::test]
async fn test_concurrent_sessions_different_configurations() {
    // Create server with multiple mock providers
    let server = TestServerBuilder::new()
        .with_mock_provider_and_voice("provider_a", "voice_a")
        .with_mock_provider_and_voice("provider_b", "voice_b")
        .with_mock_provider_and_voice("provider_c", "voice_c")
        .with_default_provider("provider_a")
        .spawn()
        .await;

    let providers = ["provider_a", "provider_b", "provider_c"];

    // Spawn sessions with different providers
    let handles: Vec<_> = (0..6)
        .map(|i| {
            let ws_url = server.ws_url();
            let provider = providers[i % 3].to_string();
            tokio::spawn(async move { run_session_with_provider(ws_url, i, &provider).await })
        })
        .collect();

    // Collect all results
    let results: Vec<SessionConfigResult> = futures_util::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.expect("Task should not panic"))
        .collect::<Result<Vec<_>, _>>()
        .expect("All sessions should complete successfully");

    // Verify all sessions completed with correct voices
    for (i, result) in results.iter().enumerate() {
        let expected_voice = format!("voice_{}", ['a', 'b', 'c'][i % 3]);
        assert_eq!(
            result.voice, expected_voice,
            "Session {} should have voice '{}', got '{}'",
            i, expected_voice, result.voice
        );
        assert!(
            result.completed,
            "Session {} should have completed successfully",
            i
        );
    }

    // Verify all session IDs are unique
    let session_ids: HashSet<_> = results.iter().map(|r| r.session_id.clone()).collect();
    assert_eq!(
        session_ids.len(),
        6,
        "All 6 sessions should have unique session IDs"
    );

    server.shutdown();
}

/// Test: Concurrent sessions with high-throughput mock provider
///
/// This test verifies the system handles concurrent sessions generating
/// multiple audio chunks per sentence
#[tokio::test]
async fn test_concurrent_sessions_multiple_chunks_per_sentence() {
    // Create provider that generates 3 chunks per sentence
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

    // Spawn concurrent sessions
    let handles: Vec<_> = (0..CONCURRENT_SESSIONS)
        .map(|i| {
            let ws_url = server.ws_url();
            tokio::spawn(async move { run_session_counting_chunks(ws_url, i).await })
        })
        .collect();

    // Collect all results
    let results: Vec<ChunkCountResult> = futures_util::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.expect("Task should not panic"))
        .collect::<Result<Vec<_>, _>>()
        .expect("All sessions should complete successfully");

    // Verify each session received the expected number of chunks
    // Each session sends 2 complete sentences + 1 trailing fragment flushed on text.done
    // = 3 sentences, each generating 3 chunks = 9 chunks total
    for (i, result) in results.iter().enumerate() {
        // 3 sentences * 3 chunks per sentence = 9 chunks
        assert_eq!(
            result.total_chunks, 9,
            "Session {} should have received 9 chunks (3 sentences * 3 chunks), got {}",
            i, result.total_chunks
        );
        assert_eq!(
            result.total_sentences, 3,
            "Session {} should have 3 sentences (2 complete + 1 flushed), got {}",
            i, result.total_sentences
        );
    }

    server.shutdown();
}

/// Test: Rapid connection/disconnection doesn't affect other sessions
///
/// This test verifies that sessions connecting and disconnecting rapidly
/// don't interfere with long-running sessions
#[tokio::test]
async fn test_concurrent_sessions_rapid_connect_disconnect() {
    let server = TestServerBuilder::new()
        .with_mock_provider("mock")
        .spawn()
        .await;

    // Start a long-running session
    let long_running = {
        let ws_url = server.ws_url();
        tokio::spawn(async move { run_long_session(ws_url).await })
    };

    // Rapidly connect and disconnect multiple short sessions
    let short_sessions: Vec<_> = (0..5)
        .map(|i| {
            let ws_url = server.ws_url();
            tokio::spawn(async move { run_short_session(ws_url, i).await })
        })
        .collect();

    // Wait for short sessions to complete
    let short_results: Vec<bool> = futures_util::future::join_all(short_sessions)
        .await
        .into_iter()
        .map(|r| r.expect("Task should not panic"))
        .collect::<Result<Vec<_>, _>>()
        .expect("Short sessions should complete");

    // All short sessions should have completed
    for (i, completed) in short_results.iter().enumerate() {
        assert!(
            *completed,
            "Short session {} should have completed successfully",
            i
        );
    }

    // Long-running session should complete successfully
    let long_result = long_running
        .await
        .expect("Task should not panic")
        .expect("Long-running session should complete");

    assert!(
        long_result.audio_done_received,
        "Long-running session should have received audio.done"
    );
    assert!(
        long_result.audio_chunks_received >= 5,
        "Long-running session should have received multiple audio chunks"
    );

    server.shutdown();
}

/// Test: All sessions receive their own session.ready message
#[tokio::test]
async fn test_concurrent_sessions_all_receive_session_ready() {
    let server = TestServerBuilder::new()
        .with_mock_provider("mock")
        .spawn()
        .await;

    // Open all connections first
    let mut connections = Vec::new();
    for _ in 0..CONCURRENT_SESSIONS {
        let ws = connect_ws(&server)
            .await
            .expect("Should connect to WebSocket");
        connections.push(ws);
    }

    // Send session.init to all concurrently
    let mut init_futures = Vec::new();
    for ws in &mut connections {
        init_futures.push(init_session(ws, Some("mock")));
    }

    // Collect all session IDs
    let session_ids: Vec<String> = futures_util::future::join_all(init_futures)
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("All sessions should initialize successfully");

    // Verify all session IDs are unique
    let unique_ids: HashSet<_> = session_ids.iter().cloned().collect();
    assert_eq!(
        unique_ids.len(),
        CONCURRENT_SESSIONS,
        "All {} sessions should have unique session IDs",
        CONCURRENT_SESSIONS
    );

    // Verify all session IDs have correct format
    for (i, session_id) in session_ids.iter().enumerate() {
        assert!(
            session_id.starts_with("ses_"),
            "Session {} ID '{}' should start with 'ses_'",
            i,
            session_id
        );
    }

    server.shutdown();
}

// ============================================================================
// Helper Types
// ============================================================================

/// Result from a basic concurrent session test
struct SessionResult {
    session_id: String,
    audio_chunks_received: usize,
    audio_done_received: bool,
}

/// Result from session isolation test
struct SessionIsolationResult {
    total_sentences: u32,
    chunk_sequences: Vec<u32>,
}

/// Result from configuration test
struct SessionConfigResult {
    session_id: String,
    voice: String,
    completed: bool,
}

/// Result from chunk counting test
struct ChunkCountResult {
    total_chunks: usize,
    total_sentences: u32,
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Run a session with unique text based on index
async fn run_session_with_unique_text(
    ws_url: String,
    index: usize,
) -> Result<SessionResult, String> {
    let (ws_stream, _) = timeout(
        CONCURRENT_TIMEOUT,
        tokio_tungstenite::connect_async(&ws_url),
    )
    .await
    .map_err(|_| "Connection timeout".to_string())?
    .map_err(|e| format!("Connection error: {e}"))?;

    let mut ws = ws_stream;

    // Initialize session
    let session_id = init_session(&mut ws, Some("mock")).await?;

    // Send unique text for this session
    // Using index to ensure each session has different content
    // Note: SRX boundary detection requires following text to emit a sentence
    let text = format!(
        "This is session number {}. Testing concurrent sessions. More text",
        index
    );

    let text_msg = ClientMessage::Text { content: text };
    send_message(&mut ws, &text_msg).await?;

    // Collect audio chunks
    let mut audio_chunks_received = 0;
    for _ in 0..20 {
        match timeout(Duration::from_millis(500), ws.next()).await {
            Ok(Some(Ok(Message::Binary(data)))) => {
                if AudioChunk::from_binary_frame(&Bytes::from(data.to_vec())).is_some() {
                    audio_chunks_received += 1;
                }
            }
            _ => break,
        }
    }

    // Finish session
    let (_, _, _) = finish_session(&mut ws).await?;

    Ok(SessionResult {
        session_id,
        audio_chunks_received,
        audio_done_received: true,
    })
}

/// Run a session with a specific number of sentences
async fn run_session_with_sentence_count(
    ws_url: String,
    index: usize,
    sentence_count: usize,
) -> Result<SessionIsolationResult, String> {
    let (ws_stream, _) = timeout(
        CONCURRENT_TIMEOUT,
        tokio_tungstenite::connect_async(&ws_url),
    )
    .await
    .map_err(|_| "Connection timeout".to_string())?
    .map_err(|e| format!("Connection error: {e}"))?;

    let mut ws = ws_stream;

    // Initialize session
    init_session(&mut ws, Some("mock")).await?;

    // Build text with specified number of sentences
    // Note: SRX boundary detection requires following text to emit a sentence,
    // so we add trailing text that will be flushed on text.done.
    // We count the trailing fragment as part of the expected sentences.
    let mut sentences: Vec<String> = (0..sentence_count)
        .map(|i| format!("Sentence {} of session {}.", i + 1, index))
        .collect();
    sentences.push("Trailing".to_string());
    let text = sentences.join(" ");

    let text_msg = ClientMessage::Text { content: text };
    send_message(&mut ws, &text_msg).await?;

    // Collect audio chunks
    let mut chunk_sequences = Vec::new();
    for _ in 0..50 {
        match timeout(Duration::from_millis(500), ws.next()).await {
            Ok(Some(Ok(Message::Binary(data)))) => {
                if let Some(chunk) = AudioChunk::from_binary_frame(&Bytes::from(data.to_vec())) {
                    chunk_sequences.push(chunk.sequence);
                }
            }
            _ => break,
        }
    }

    // Finish session and get total sentences
    let (total_sentences, _, _) = finish_session(&mut ws).await?;

    Ok(SessionIsolationResult {
        total_sentences,
        chunk_sequences,
    })
}

/// Run a session with a specific provider
async fn run_session_with_provider(
    ws_url: String,
    _index: usize,
    provider: &str,
) -> Result<SessionConfigResult, String> {
    let (ws_stream, _) = timeout(
        CONCURRENT_TIMEOUT,
        tokio_tungstenite::connect_async(&ws_url),
    )
    .await
    .map_err(|_| "Connection timeout".to_string())?
    .map_err(|e| format!("Connection error: {e}"))?;

    let mut ws = ws_stream;

    // Initialize session with specific provider and capture voice
    let init_msg = ClientMessage::SessionInit {
        provider: Some(provider.to_string()),
        voice: None,
        audio_format: None,
        code_block_mode: None,
    };
    send_message(&mut ws, &init_msg).await?;

    // Wait for session.ready
    let response = timeout(DEFAULT_TIMEOUT, receive_server_message(&mut ws))
        .await
        .map_err(|_| "Receive timeout".to_string())??;

    let (session_id, voice) = match response {
        ServerMessage::SessionReady {
            session_id, voice, ..
        } => (session_id, voice),
        other => return Err(format!("Expected SessionReady, got: {other:?}")),
    };

    // Send text
    // Note: SRX boundary detection requires following text to emit a sentence
    let text_msg = ClientMessage::Text {
        content: "Test sentence. More text".to_string(),
    };
    send_message(&mut ws, &text_msg).await?;

    // Consume audio
    for _ in 0..10 {
        match timeout(Duration::from_millis(300), ws.next()).await {
            Ok(Some(Ok(Message::Binary(_)))) => {}
            _ => break,
        }
    }

    // Finish session
    finish_session(&mut ws).await?;

    Ok(SessionConfigResult {
        session_id,
        voice,
        completed: true,
    })
}

/// Run a session and count audio chunks
async fn run_session_counting_chunks(
    ws_url: String,
    _index: usize,
) -> Result<ChunkCountResult, String> {
    let (ws_stream, _) = timeout(
        CONCURRENT_TIMEOUT,
        tokio_tungstenite::connect_async(&ws_url),
    )
    .await
    .map_err(|_| "Connection timeout".to_string())?
    .map_err(|e| format!("Connection error: {e}"))?;

    let mut ws = ws_stream;

    // Initialize session
    init_session(&mut ws, Some("multi")).await?;

    // Send text with 2 sentences
    // Note: SRX boundary detection requires following text to emit a sentence,
    // so we add trailing text that will be flushed on text.done
    let text_msg = ClientMessage::Text {
        content: "First sentence. Second sentence. Trailing".to_string(),
    };
    send_message(&mut ws, &text_msg).await?;

    // Count chunks for the 2 complete sentences (before text.done)
    let mut total_chunks = 0;
    for _ in 0..30 {
        match timeout(Duration::from_millis(500), ws.next()).await {
            Ok(Some(Ok(Message::Binary(data)))) => {
                if AudioChunk::from_binary_frame(&Bytes::from(data.to_vec())).is_some() {
                    total_chunks += 1;
                }
            }
            _ => break,
        }
    }

    // Send text.done to flush the trailing fragment
    send_message(&mut ws, &ClientMessage::TextDone).await?;

    // Collect remaining chunks (from flushed fragment) and wait for audio.done
    let mut audio_done_received = false;
    let mut total_sentences = 0u32;
    for _ in 0..50 {
        let msg = match timeout(DEFAULT_TIMEOUT, ws.next()).await {
            Ok(Some(Ok(msg))) => msg,
            Ok(Some(Err(e))) => return Err(format!("Receive error: {e}")),
            Ok(None) => return Err("Connection closed".to_string()),
            Err(_) => return Err("Timeout waiting for audio.done".to_string()),
        };

        match msg {
            Message::Binary(data) => {
                if AudioChunk::from_binary_frame(&Bytes::from(data.to_vec())).is_some() {
                    total_chunks += 1;
                }
            }
            Message::Text(text) => {
                let server_msg: ServerMessage =
                    serde_json::from_str(&text).map_err(|e| format!("Parse error: {e}"))?;
                if let ServerMessage::AudioDone {
                    total_sentences: ts,
                    ..
                } = server_msg
                {
                    total_sentences = ts;
                    audio_done_received = true;
                    break;
                }
            }
            _ => {}
        }
    }

    if !audio_done_received {
        return Err("Did not receive audio.done".to_string());
    }

    Ok(ChunkCountResult {
        total_chunks,
        total_sentences,
    })
}

/// Run a long session that sends multiple text chunks
async fn run_long_session(ws_url: String) -> Result<SessionResult, String> {
    let (ws_stream, _) = timeout(
        CONCURRENT_TIMEOUT,
        tokio_tungstenite::connect_async(&ws_url),
    )
    .await
    .map_err(|_| "Connection timeout".to_string())?
    .map_err(|e| format!("Connection error: {e}"))?;

    let mut ws = ws_stream;

    // Initialize session
    let session_id = init_session(&mut ws, Some("mock")).await?;

    let mut audio_chunks_received = 0;

    // Send multiple text chunks to simulate a longer session
    for i in 0..5 {
        // Note: SRX boundary detection requires following text to emit a sentence
        let text = format!("Long running sentence number {}. Continuing", i + 1);
        let text_msg = ClientMessage::Text { content: text };
        send_message(&mut ws, &text_msg).await?;

        // Small delay between sends
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Consume any available audio and count it
        for _ in 0..5 {
            match timeout(Duration::from_millis(100), ws.next()).await {
                Ok(Some(Ok(Message::Binary(data)))) => {
                    if AudioChunk::from_binary_frame(&Bytes::from(data.to_vec())).is_some() {
                        audio_chunks_received += 1;
                    }
                }
                _ => break,
            }
        }
    }

    // Collect remaining audio
    for _ in 0..50 {
        match timeout(Duration::from_millis(300), ws.next()).await {
            Ok(Some(Ok(Message::Binary(data)))) => {
                if AudioChunk::from_binary_frame(&Bytes::from(data.to_vec())).is_some() {
                    audio_chunks_received += 1;
                }
            }
            _ => break,
        }
    }

    // Finish session
    finish_session(&mut ws).await?;

    Ok(SessionResult {
        session_id,
        audio_chunks_received,
        audio_done_received: true,
    })
}

/// Run a short session that connects, initializes, and immediately finishes
async fn run_short_session(ws_url: String, _index: usize) -> Result<bool, String> {
    let (ws_stream, _) = timeout(
        CONCURRENT_TIMEOUT,
        tokio_tungstenite::connect_async(&ws_url),
    )
    .await
    .map_err(|_| "Connection timeout".to_string())?
    .map_err(|e| format!("Connection error: {e}"))?;

    let mut ws = ws_stream;

    // Initialize session
    init_session(&mut ws, Some("mock")).await?;

    // Send minimal text
    // Note: SRX boundary detection requires following text to emit a sentence
    let text_msg = ClientMessage::Text {
        content: "Quick test. Done".to_string(),
    };
    send_message(&mut ws, &text_msg).await?;

    // Consume audio quickly
    for _ in 0..5 {
        match timeout(Duration::from_millis(100), ws.next()).await {
            Ok(Some(Ok(Message::Binary(_)))) => {}
            _ => break,
        }
    }

    // Finish session
    finish_session(&mut ws).await?;

    Ok(true)
}

/// Receive and parse a server message (local helper)
async fn receive_server_message<S>(ws: &mut S) -> Result<ServerMessage, String>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    match ws.next().await {
        Some(Ok(Message::Text(text))) => {
            serde_json::from_str(&text).map_err(|e| format!("Parse error: {e}"))
        }
        Some(Ok(other)) => Err(format!("Expected text message, got: {other:?}")),
        Some(Err(e)) => Err(format!("Receive error: {e}")),
        None => Err("Connection closed".to_string()),
    }
}
