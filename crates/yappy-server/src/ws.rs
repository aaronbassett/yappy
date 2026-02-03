//! WebSocket upgrade handler and message loop
//!
//! This module provides the WebSocket endpoint for the Yappy TTS streaming protocol.
//! It handles WebSocket upgrade requests, manages the connection lifecycle, and
//! provides the basic message handling loop structure.
//!
//! # Protocol Overview
//!
//! The WebSocket connection follows this lifecycle:
//! 1. Client initiates WebSocket upgrade at `/ws`
//! 2. Server accepts the upgrade and starts the message loop
//! 3. Client sends `session.init` to initialize TTS session
//! 4. Client sends `text` messages with content to synthesize
//! 5. Server sends binary audio frames and control messages
//! 6. Client sends `text.done` to signal end of input
//! 7. Server sends `audio.done` and connection can be closed
//!
//! # Privacy
//!
//! Per project requirements, text content is never logged. Only metadata such as
//! message types, frame sizes, and connection events are recorded.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use tracing::{debug, info, instrument, warn};
use yappy_core::{ClientMessage, ServerMessage};

use crate::state::AppState;

/// WebSocket upgrade handler for the `/ws` endpoint.
///
/// This handler performs the WebSocket protocol upgrade and spawns the
/// connection handler. It accepts the following extractors:
/// - `State<AppState>` - Application state with providers and config
/// - `WebSocketUpgrade` - Axum extractor that handles the upgrade
///
/// # Tracing
///
/// Creates a span for the WebSocket connection with connection metadata.
/// Individual message handling is logged at debug level without content.
///
/// # Example
///
/// This handler is typically registered in the router:
///
/// ```ignore
/// Router::new()
///     .route("/ws", any(ws_upgrade_handler))
///     .with_state(state)
/// ```
#[instrument(name = "ws_upgrade", skip_all)]
pub async fn ws_upgrade_handler(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    info!("WebSocket upgrade requested");

    // Accept the WebSocket upgrade and spawn the handler
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Handle an established WebSocket connection.
///
/// This function manages the WebSocket message loop after the connection
/// has been upgraded. It:
/// - Splits the socket into sender and receiver halves
/// - Processes incoming messages (text and binary)
/// - Handles connection close gracefully
/// - Logs connection lifecycle events
///
/// # Arguments
///
/// * `socket` - The upgraded WebSocket connection
/// * `state` - Application state for accessing providers
///
/// # Message Handling
///
/// Currently implements a basic echo/logging loop. Full message processing
/// (session initialization, text synthesis) will be added in later tasks.
#[instrument(name = "ws_connection", skip_all, fields(remote_addr))]
async fn handle_socket(socket: WebSocket, _state: AppState) {
    info!("WebSocket connection established");

    // Split the socket into sender and receiver for independent handling
    let (mut sender, mut receiver) = socket.split();

    // Process incoming messages
    while let Some(result) = receiver.next().await {
        match result {
            Ok(message) => {
                if !process_message(message, &mut sender).await {
                    // Connection should be closed
                    break;
                }
            }
            Err(err) => {
                // Log connection errors without exposing internal details
                warn!("WebSocket receive error: {}", err);
                break;
            }
        }
    }

    info!("WebSocket connection closed");
}

/// Process a single WebSocket message.
///
/// Handles different message types:
/// - Text frames: Parsed as JSON `ClientMessage` (logged without content)
/// - Binary frames: Logged with size (content not logged)
/// - Ping: Axum handles pong automatically
/// - Pong: Ignored
/// - Close: Signals connection should close
///
/// # Returns
///
/// Returns `true` if the message loop should continue, `false` if the
/// connection should be closed.
///
/// # Arguments
///
/// * `message` - The received WebSocket message
/// * `sender` - Mutable reference to the sender half for responses
async fn process_message<S>(message: Message, sender: &mut S) -> bool
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    match message {
        Message::Text(text) => handle_text_message(&text, sender).await,
        Message::Binary(data) => handle_binary_message(&data, sender).await,
        Message::Ping(_) => {
            // Axum automatically responds to pings with pongs
            debug!("Received ping");
            true
        }
        Message::Pong(_) => {
            // Pong frames are usually responses to our pings; ignore them
            debug!("Received pong");
            true
        }
        Message::Close(frame) => {
            if let Some(cf) = &frame {
                info!(code = %cf.code, "Received close frame");
            } else {
                info!("Received close frame (no reason)");
            }
            false
        }
    }
}

/// Handle a text WebSocket frame.
///
/// Parses the text as a JSON `ClientMessage` and logs the message type.
/// For now, responds with an acknowledgment. Full message handling will
/// be implemented in later tasks.
///
/// # Privacy
///
/// The message content is parsed but never logged. Only the message type
/// is recorded for observability.
///
/// # Returns
///
/// Returns `true` to continue the message loop, `false` on fatal errors.
async fn handle_text_message<S>(text: &str, sender: &mut S) -> bool
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    // Attempt to parse as ClientMessage
    match serde_json::from_str::<ClientMessage>(text) {
        Ok(client_msg) => {
            // Log message type only, not content (privacy requirement)
            match &client_msg {
                ClientMessage::SessionInit { provider, .. } => {
                    debug!(
                        message_type = "session.init",
                        provider = provider.as_deref().unwrap_or("default"),
                        "Received client message"
                    );

                    // For now, send a placeholder error indicating not yet implemented
                    // Full session handling will be added in later tasks
                    let response = ServerMessage::session_error(
                        "not_implemented",
                        "Session initialization not yet implemented",
                    );
                    if let Err(err) = send_server_message(sender, &response).await {
                        warn!("Failed to send response: {}", err);
                        return false;
                    }
                }
                ClientMessage::Text { .. } => {
                    debug!(message_type = "text", "Received client message");

                    // For now, send an error since we don't have session handling yet
                    let response = ServerMessage::error(
                        "no_session",
                        "No active session - send session.init first",
                    );
                    if let Err(err) = send_server_message(sender, &response).await {
                        warn!("Failed to send response: {}", err);
                        return false;
                    }
                }
                ClientMessage::TextDone => {
                    debug!(message_type = "text.done", "Received client message");

                    // For now, send an error since we don't have session handling yet
                    let response = ServerMessage::error(
                        "no_session",
                        "No active session - send session.init first",
                    );
                    if let Err(err) = send_server_message(sender, &response).await {
                        warn!("Failed to send response: {}", err);
                        return false;
                    }
                }
            }
            true
        }
        Err(err) => {
            // Invalid JSON or unknown message type
            warn!(error = %err, "Failed to parse client message");

            let response =
                ServerMessage::error("invalid_message", "Failed to parse message as valid JSON");
            if let Err(send_err) = send_server_message(sender, &response).await {
                warn!("Failed to send error response: {}", send_err);
                return false;
            }
            true
        }
    }
}

/// Handle a binary WebSocket frame.
///
/// Binary frames from clients are not expected in the current protocol
/// (clients send text JSON, server sends binary audio). Logs the frame
/// size and sends an error response.
///
/// # Returns
///
/// Returns `true` to continue the message loop.
async fn handle_binary_message<S>(data: &[u8], sender: &mut S) -> bool
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    debug!(size = data.len(), "Received unexpected binary frame");

    let response = ServerMessage::error(
        "unexpected_binary",
        "Binary frames from client are not supported",
    );
    if let Err(err) = send_server_message(sender, &response).await {
        warn!("Failed to send error response: {}", err);
        return false;
    }
    true
}

/// Send a `ServerMessage` as a JSON text frame.
///
/// Serializes the server message to JSON and sends it over the WebSocket.
///
/// # Arguments
///
/// * `sender` - The sender half of the WebSocket
/// * `message` - The server message to send
///
/// # Errors
///
/// Returns an error if serialization fails or the send fails.
async fn send_server_message<S>(sender: &mut S, message: &ServerMessage) -> Result<(), String>
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    let json =
        serde_json::to_string(message).map_err(|e| format!("JSON serialization failed: {e}"))?;

    sender
        .send(Message::Text(json.into()))
        .await
        .map_err(|e| format!("Send failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    /// Mock sink for testing message sending
    struct MockSink {
        messages: Vec<Message>,
    }

    impl MockSink {
        fn new() -> Self {
            Self {
                messages: Vec::new(),
            }
        }
    }

    impl futures_util::Sink<Message> for MockSink {
        type Error = std::io::Error;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
            self.messages.push(item);
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn test_handle_text_message_session_init() {
        let mut sink = MockSink::new();
        let text = r#"{"type":"session.init","provider":"kokoro"}"#;

        let should_continue = handle_text_message(text, &mut sink).await;

        assert!(should_continue);
        assert_eq!(sink.messages.len(), 1);

        // Verify it's a session.error response (not implemented yet)
        if let Message::Text(json) = &sink.messages[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            assert!(matches!(msg, ServerMessage::SessionError { .. }));
        } else {
            panic!("Expected text message");
        }
    }

    #[tokio::test]
    async fn test_handle_text_message_text() {
        let mut sink = MockSink::new();
        let text = r#"{"type":"text","content":"Hello world"}"#;

        let should_continue = handle_text_message(text, &mut sink).await;

        assert!(should_continue);
        assert_eq!(sink.messages.len(), 1);

        // Verify it's an error response (no session)
        if let Message::Text(json) = &sink.messages[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            assert!(matches!(msg, ServerMessage::Error { .. }));
        } else {
            panic!("Expected text message");
        }
    }

    #[tokio::test]
    async fn test_handle_text_message_invalid_json() {
        let mut sink = MockSink::new();
        let text = "not valid json";

        let should_continue = handle_text_message(text, &mut sink).await;

        assert!(should_continue);
        assert_eq!(sink.messages.len(), 1);

        // Verify it's an error response
        if let Message::Text(json) = &sink.messages[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            if let ServerMessage::Error { code, .. } = msg {
                assert_eq!(code, "invalid_message");
            } else {
                panic!("Expected Error message");
            }
        } else {
            panic!("Expected text message");
        }
    }

    #[tokio::test]
    async fn test_handle_binary_message() {
        let mut sink = MockSink::new();
        let data = vec![0u8; 100];

        let should_continue = handle_binary_message(&data, &mut sink).await;

        assert!(should_continue);
        assert_eq!(sink.messages.len(), 1);

        // Verify it's an error response
        if let Message::Text(json) = &sink.messages[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            if let ServerMessage::Error { code, .. } = msg {
                assert_eq!(code, "unexpected_binary");
            } else {
                panic!("Expected Error message");
            }
        } else {
            panic!("Expected text message");
        }
    }

    #[tokio::test]
    async fn test_process_message_close() {
        let mut sink = MockSink::new();
        let message = Message::Close(None);

        let should_continue = process_message(message, &mut sink).await;

        assert!(!should_continue);
    }

    #[tokio::test]
    async fn test_process_message_ping() {
        let mut sink = MockSink::new();
        let message = Message::Ping(vec![1, 2, 3].into());

        let should_continue = process_message(message, &mut sink).await;

        assert!(should_continue);
        // No response expected (Axum handles pong automatically)
        assert!(sink.messages.is_empty());
    }

    #[tokio::test]
    async fn test_send_server_message() {
        let mut sink = MockSink::new();
        let message = ServerMessage::error("test", "Test error");

        let result = send_server_message(&mut sink, &message).await;

        assert!(result.is_ok());
        assert_eq!(sink.messages.len(), 1);

        if let Message::Text(json) = &sink.messages[0] {
            assert!(json.contains(r#""type":"error""#));
            assert!(json.contains(r#""code":"test""#));
        } else {
            panic!("Expected text message");
        }
    }
}
