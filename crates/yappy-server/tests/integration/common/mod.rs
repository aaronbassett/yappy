//! Common test utilities for integration tests
//!
//! This module provides:
//! - [`mock`] - Mock TTS provider for testing
//! - [`server`] - Test server helpers

pub mod mock;
pub mod server;

// Re-export commonly used items
pub use mock::{MockSynthesisMode, MockTtsProvider};
pub use server::{
    connect_ws, connect_ws_with_timeout, spawn_empty_server, spawn_test_server, TestServer,
    TestServerBuilder,
};

use std::time::Duration;

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use yappy_core::{AudioChunk, ClientMessage, ServerMessage};

/// Default timeout for WebSocket operations in tests
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Send a client message to a WebSocket stream
pub async fn send_message<S>(ws: &mut S, msg: &ClientMessage) -> Result<(), String>
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    let json = serde_json::to_string(msg).map_err(|e| format!("Serialization error: {}", e))?;
    ws.send(Message::Text(json.into()))
        .await
        .map_err(|e| format!("Send error: {}", e))
}

/// Receive the next text message from a WebSocket stream
pub async fn receive_text<S>(ws: &mut S) -> Result<String, String>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    match ws.next().await {
        Some(Ok(Message::Text(text))) => Ok(text.to_string()),
        Some(Ok(other)) => Err(format!("Expected text message, got: {:?}", other)),
        Some(Err(e)) => Err(format!("Receive error: {}", e)),
        None => Err("Connection closed".to_string()),
    }
}

/// Receive the next binary message from a WebSocket stream
pub async fn receive_binary<S>(ws: &mut S) -> Result<Vec<u8>, String>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    match ws.next().await {
        Some(Ok(Message::Binary(data))) => Ok(data.to_vec()),
        Some(Ok(other)) => Err(format!("Expected binary message, got: {:?}", other)),
        Some(Err(e)) => Err(format!("Receive error: {}", e)),
        None => Err("Connection closed".to_string()),
    }
}

/// Receive and parse a server message
pub async fn receive_server_message<S>(ws: &mut S) -> Result<ServerMessage, String>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let text = receive_text(ws).await?;
    serde_json::from_str(&text).map_err(|e| format!("Parse error: {} (text: {})", e, text))
}

/// Receive and parse a server message with timeout
pub async fn receive_server_message_with_timeout<S>(
    ws: &mut S,
    timeout_duration: Duration,
) -> Result<ServerMessage, String>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    timeout(timeout_duration, receive_server_message(ws))
        .await
        .map_err(|_| "Receive timeout".to_string())?
}

/// Receive and parse a binary audio chunk with timeout
pub async fn receive_audio_chunk_with_timeout<S>(
    ws: &mut S,
    timeout_duration: Duration,
) -> Result<AudioChunk, String>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let data = timeout(timeout_duration, receive_binary(ws))
        .await
        .map_err(|_| "Receive timeout".to_string())??;

    AudioChunk::from_binary_frame(&Bytes::from(data))
        .ok_or_else(|| "Invalid audio frame".to_string())
}

/// Send session.init and wait for session.ready
///
/// Returns the session_id from the session.ready response.
pub async fn init_session<S>(ws: &mut S, provider: Option<&str>) -> Result<String, String>
where
    S: SinkExt<Message>
        + StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + Unpin,
    S::Error: std::fmt::Display,
{
    // Send session.init
    let init_msg = ClientMessage::SessionInit {
        provider: provider.map(String::from),
        voice: None,
        audio_format: None,
        code_block_mode: None,
    };
    send_message(ws, &init_msg).await?;

    // Wait for session.ready
    let response = receive_server_message_with_timeout(ws, DEFAULT_TIMEOUT).await?;

    match response {
        ServerMessage::SessionReady { session_id, .. } => Ok(session_id),
        ServerMessage::SessionError { code, message, .. } => {
            Err(format!("Session error [{}]: {}", code, message))
        }
        other => Err(format!("Unexpected response: {:?}", other)),
    }
}

/// Send text and collect all resulting audio chunks
///
/// Returns a vector of audio chunks received.
pub async fn send_text_and_collect_audio<S>(
    ws: &mut S,
    text: &str,
    max_chunks: usize,
) -> Result<Vec<AudioChunk>, String>
where
    S: SinkExt<Message>
        + StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + Unpin,
    S::Error: std::fmt::Display,
{
    // Send text message
    let text_msg = ClientMessage::Text {
        content: text.to_string(),
    };
    send_message(ws, &text_msg).await?;

    // Collect audio chunks
    let mut chunks = Vec::new();
    for _ in 0..max_chunks {
        // Use a short timeout since we don't know how many chunks to expect
        match timeout(Duration::from_millis(500), receive_binary(ws)).await {
            Ok(Ok(data)) => {
                if let Some(chunk) = AudioChunk::from_binary_frame(&Bytes::from(data)) {
                    chunks.push(chunk);
                } else {
                    // Not a valid audio chunk, might be a text message
                    break;
                }
            }
            Ok(Err(_)) | Err(_) => break,
        }
    }

    Ok(chunks)
}

/// Send text.done and wait for audio.done
///
/// Returns the audio.done statistics.
pub async fn finish_session<S>(ws: &mut S) -> Result<(u32, u64, u64), String>
where
    S: SinkExt<Message>
        + StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + Unpin,
    S::Error: std::fmt::Display,
{
    // Send text.done
    send_message(ws, &ClientMessage::TextDone).await?;

    // Collect any remaining audio chunks and wait for audio.done
    loop {
        let msg = timeout(DEFAULT_TIMEOUT, ws.next())
            .await
            .map_err(|_| "Timeout waiting for audio.done".to_string())?
            .ok_or("Connection closed".to_string())?
            .map_err(|e| format!("Receive error: {}", e))?;

        match msg {
            Message::Binary(_) => {
                // Skip audio chunks (they might still be arriving)
                continue;
            }
            Message::Text(text) => {
                let server_msg: ServerMessage =
                    serde_json::from_str(&text).map_err(|e| format!("Parse error: {}", e))?;

                match server_msg {
                    ServerMessage::AudioDone {
                        total_sentences,
                        total_duration_ms,
                        total_bytes,
                    } => {
                        return Ok((total_sentences, total_duration_ms, total_bytes));
                    }
                    ServerMessage::Error { code, message, .. } => {
                        // Non-fatal error, continue waiting
                        eprintln!("Warning: received error [{}]: {}", code, message);
                        continue;
                    }
                    ServerMessage::SessionError { code, message, .. } => {
                        return Err(format!("Session error [{}]: {}", code, message));
                    }
                    _ => {
                        return Err(format!("Unexpected message: {:?}", server_msg));
                    }
                }
            }
            Message::Close(_) => {
                return Err("Connection closed before audio.done".to_string());
            }
            _ => continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_timeout() {
        assert_eq!(DEFAULT_TIMEOUT, Duration::from_secs(5));
    }
}
