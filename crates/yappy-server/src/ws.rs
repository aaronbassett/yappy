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
use yappy_core::provider::ProviderId;
use yappy_core::{AudioFormat, ClientMessage, CodeBlockMode, ServerMessage, Session, VoiceConfig};

use crate::state::{AppState, ProviderRegistry};

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
/// - Tracks session state for the connection
/// - Processes incoming messages (text and binary)
/// - Handles connection close gracefully
/// - Logs connection lifecycle events
///
/// # Arguments
///
/// * `socket` - The upgraded WebSocket connection
/// * `state` - Application state for accessing providers
///
/// # Session Lifecycle
///
/// Each WebSocket connection can have at most one session. The session is
/// created when the client sends a `session.init` message and remains active
/// until the connection closes.
#[instrument(name = "ws_connection", skip_all, fields(remote_addr))]
async fn handle_socket(socket: WebSocket, state: AppState) {
    info!("WebSocket connection established");

    // Split the socket into sender and receiver for independent handling
    let (mut sender, mut receiver) = socket.split();

    // Session state for this connection (None until session.init is received)
    let mut session: Option<Session> = None;

    // Process incoming messages
    while let Some(result) = receiver.next().await {
        match result {
            Ok(message) => {
                if !process_message(message, &mut sender, &mut session, state.providers()).await {
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

    if let Some(ref s) = session {
        info!(session_id = %s.id, "WebSocket connection closed");
    } else {
        info!("WebSocket connection closed (no session)");
    }
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
/// * `session` - Mutable reference to the current session state
/// * `providers` - Reference to the provider registry
async fn process_message<S>(
    message: Message,
    sender: &mut S,
    session: &mut Option<Session>,
    providers: &ProviderRegistry,
) -> bool
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    match message {
        Message::Text(text) => handle_text_message(&text, sender, session, providers).await,
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
/// Parses the text as a JSON `ClientMessage` and routes to the appropriate
/// handler based on message type. For session.init, performs provider and
/// voice resolution before creating the session.
///
/// # Privacy
///
/// The message content is parsed but never logged. Only the message type
/// is recorded for observability.
///
/// # Returns
///
/// Returns `true` to continue the message loop, `false` on fatal errors.
async fn handle_text_message<S>(
    text: &str,
    sender: &mut S,
    session: &mut Option<Session>,
    providers: &ProviderRegistry,
) -> bool
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    // Attempt to parse as ClientMessage
    match serde_json::from_str::<ClientMessage>(text) {
        Ok(client_msg) => {
            // Log message type only, not content (privacy requirement)
            match client_msg {
                ClientMessage::SessionInit {
                    provider,
                    voice,
                    audio_format,
                    code_block_mode,
                } => {
                    debug!(
                        message_type = "session.init",
                        provider = provider.as_deref().unwrap_or("default"),
                        "Received client message"
                    );

                    handle_session_init(
                        sender,
                        session,
                        providers,
                        provider,
                        voice,
                        audio_format,
                        code_block_mode,
                    )
                    .await
                }
                ClientMessage::Text { .. } => {
                    debug!(message_type = "text", "Received client message");

                    // Check if session is initialized
                    if session.is_none() {
                        let response = ServerMessage::error(
                            "no_session",
                            "No active session - send session.init first",
                        );
                        if let Err(err) = send_server_message(sender, &response).await {
                            warn!("Failed to send response: {}", err);
                            return false;
                        }
                    } else {
                        // Text message handling will be implemented in a later task
                        let response = ServerMessage::error(
                            "not_implemented",
                            "Text processing not yet implemented",
                        );
                        if let Err(err) = send_server_message(sender, &response).await {
                            warn!("Failed to send response: {}", err);
                            return false;
                        }
                    }
                    true
                }
                ClientMessage::TextDone => {
                    debug!(message_type = "text.done", "Received client message");

                    // Check if session is initialized
                    if session.is_none() {
                        let response = ServerMessage::error(
                            "no_session",
                            "No active session - send session.init first",
                        );
                        if let Err(err) = send_server_message(sender, &response).await {
                            warn!("Failed to send response: {}", err);
                            return false;
                        }
                    } else {
                        // text.done handling will be implemented in a later task
                        let response = ServerMessage::error(
                            "not_implemented",
                            "text.done processing not yet implemented",
                        );
                        if let Err(err) = send_server_message(sender, &response).await {
                            warn!("Failed to send response: {}", err);
                            return false;
                        }
                    }
                    true
                }
            }
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

/// Handle session.init message and create a new session.
///
/// This function:
/// 1. Checks if a session already exists (error if so)
/// 2. Resolves the provider (specified or default)
/// 3. Validates provider availability via health check
/// 4. Resolves voice configuration
/// 5. Resolves audio format
/// 6. Creates the session and sends session.ready
///
/// # Error Handling
///
/// - `session_already_initialized`: If session already exists
/// - `no_providers`: If no providers are registered
/// - `provider_unavailable`: If specified/default provider is not available
///
/// # Returns
///
/// Returns `true` to continue the message loop, `false` on fatal errors
/// that require closing the connection.
#[allow(clippy::too_many_arguments)]
async fn handle_session_init<S>(
    sender: &mut S,
    session: &mut Option<Session>,
    providers: &ProviderRegistry,
    requested_provider: Option<String>,
    requested_voice: Option<VoiceConfig>,
    requested_format: Option<AudioFormat>,
    requested_code_block_mode: Option<CodeBlockMode>,
) -> bool
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    // Check if session already exists
    if session.is_some() {
        let response = ServerMessage::error(
            "session_already_initialized",
            "Session has already been initialized for this connection",
        );
        if let Err(err) = send_server_message(sender, &response).await {
            warn!("Failed to send response: {err}");
            return false;
        }
        return true;
    }

    // Check if any providers are registered
    if providers.is_empty() {
        let response = ServerMessage::session_error(
            "no_providers",
            "No TTS providers are available on this server",
        );
        if let Err(err) = send_server_message(sender, &response).await {
            warn!("Failed to send response: {err}");
        }
        // Fatal error - close connection after sending
        return false;
    }

    // Resolve provider and validate it
    let Some((provider_id, provider)) =
        resolve_provider(sender, providers, requested_provider.as_deref()).await
    else {
        return false; // Error already sent
    };

    // Get provider metadata for defaults
    let metadata = provider.metadata();

    // Resolve voice configuration
    let voice = requested_voice.unwrap_or_else(|| {
        let default_voice_id = metadata
            .voices
            .first()
            .map(|v| v.id.clone())
            .unwrap_or_default();
        VoiceConfig {
            id: default_voice_id,
            ..Default::default()
        }
    });

    // Resolve audio format
    let audio_format = requested_format.unwrap_or_else(|| {
        metadata
            .supported_formats
            .first()
            .cloned()
            .unwrap_or_default()
    });

    // Resolve code block mode
    let code_block_mode = requested_code_block_mode.unwrap_or_default();

    // Create the session
    let new_session = Session::new(
        provider_id,
        voice.clone(),
        audio_format.clone(),
        code_block_mode,
    );
    let session_id = new_session.id.clone();

    info!(
        session_id = %session_id,
        provider = %new_session.provider_id,
        voice = %voice.id,
        audio_codec = %audio_format.codec,
        "Session initialized"
    );

    // Send session.ready response
    let response = ServerMessage::session_ready(&session_id, audio_format, &voice.id);
    if let Err(err) = send_server_message(sender, &response).await {
        warn!("Failed to send session.ready: {err}");
        return false;
    }

    // Store the session
    *session = Some(new_session);

    true
}

/// Resolve and validate the requested provider.
///
/// Returns `Some((provider_id, provider))` on success, or `None` if an error
/// was sent to the client (fatal error, connection should be closed).
async fn resolve_provider<S>(
    sender: &mut S,
    providers: &ProviderRegistry,
    requested_provider: Option<&str>,
) -> Option<(
    ProviderId,
    std::sync::Arc<dyn yappy_core::TtsProvider + Send + Sync>,
)>
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    // Resolve the provider ID (use requested or default)
    let provider_id = if let Some(name) = requested_provider {
        ProviderId::new(name)
    } else if let Some(id) = providers.default_provider() {
        id
    } else {
        // No default set, but we have providers - use first available
        let available = providers.available_providers().await;
        if let Some(first) = available.into_iter().next() {
            first
        } else {
            // All providers are unavailable
            let all_ids: Vec<String> = providers.list().into_iter().map(|m| m.id.0).collect();
            let response = ServerMessage::session_error_with_alternatives(
                "provider_unavailable",
                "No TTS providers are currently available",
                all_ids,
            );
            if let Err(err) = send_server_message(sender, &response).await {
                warn!("Failed to send response: {err}");
            }
            return None;
        }
    };

    // Get the provider
    let Some(provider) = providers.get(&provider_id) else {
        // Provider not found - send error with alternatives
        let available = providers.available_providers().await;
        let alternatives: Vec<String> = available.into_iter().map(|id| id.0).collect();
        let response = ServerMessage::session_error_with_alternatives(
            "provider_unavailable",
            format!("Provider '{provider_id}' is not available"),
            alternatives,
        );
        if let Err(err) = send_server_message(sender, &response).await {
            warn!("Failed to send response: {err}");
        }
        return None;
    };

    // Check provider health
    let status = provider.health_check().await;
    if !status.is_available() {
        let available = providers.available_providers().await;
        let alternatives: Vec<String> = available.into_iter().map(|id| id.0).collect();
        let reason = match status {
            yappy_core::ProviderStatus::NotConfigured { reason }
            | yappy_core::ProviderStatus::Unavailable { reason } => reason,
            yappy_core::ProviderStatus::Available => unreachable!(),
        };
        let response = ServerMessage::session_error_with_alternatives(
            "provider_unavailable",
            format!("Provider '{provider_id}' is unavailable: {reason}"),
            alternatives,
        );
        if let Err(err) = send_server_message(sender, &response).await {
            warn!("Failed to send response: {err}");
        }
        return None;
    }

    Some((provider_id, provider))
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
    use async_trait::async_trait;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio_util::sync::CancellationToken;
    use yappy_core::{
        audio::AudioStream,
        error::ProviderError,
        provider::{ProviderMetadata, VoiceInfo},
        ProviderStatus, TtsProvider,
    };

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

    /// Mock TTS provider for testing
    struct MockProvider {
        id: String,
        name: String,
        status: ProviderStatus,
        voices: Vec<VoiceInfo>,
    }

    impl MockProvider {
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
            }
        }

        fn unavailable(id: &str, name: &str, reason: &str) -> Self {
            Self {
                id: id.to_string(),
                name: name.to_string(),
                status: ProviderStatus::Unavailable {
                    reason: reason.to_string(),
                },
                voices: vec![],
            }
        }
    }

    #[async_trait]
    impl TtsProvider for MockProvider {
        fn metadata(&self) -> ProviderMetadata {
            ProviderMetadata {
                id: ProviderId::new(&self.id),
                name: self.name.clone(),
                description: format!("Mock {} provider", self.name),
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
            _cancel: CancellationToken,
        ) -> Result<AudioStream, ProviderError> {
            unimplemented!("Mock provider does not synthesize")
        }
    }

    /// Helper to create a provider registry with a mock provider
    fn create_test_registry() -> ProviderRegistry {
        let mut registry = ProviderRegistry::new();
        registry.register(MockProvider::new("test", "Test Provider"));
        registry.set_default(ProviderId::new("test"));
        registry
    }

    /// Helper to create an empty provider registry
    fn create_empty_registry() -> ProviderRegistry {
        ProviderRegistry::new()
    }

    // ==================== Session Init Tests ====================

    #[tokio::test]
    async fn test_session_init_success_with_default_provider() {
        let mut sink = MockSink::new();
        let mut session = None;
        let registry = create_test_registry();
        let text = r#"{"type":"session.init"}"#;

        let should_continue = handle_text_message(text, &mut sink, &mut session, &registry).await;

        assert!(should_continue);
        assert!(session.is_some());
        assert_eq!(sink.messages.len(), 1);

        // Verify it's a session.ready response
        if let Message::Text(json) = &sink.messages[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            match msg {
                ServerMessage::SessionReady {
                    session_id,
                    voice,
                    audio_format,
                } => {
                    assert!(session_id.starts_with("ses_"));
                    assert_eq!(voice, "test_voice");
                    assert_eq!(audio_format.codec, yappy_core::AudioCodec::Opus);
                }
                _ => panic!("Expected SessionReady, got {:?}", msg),
            }
        } else {
            panic!("Expected text message");
        }
    }

    #[tokio::test]
    async fn test_session_init_success_with_specified_provider() {
        let mut sink = MockSink::new();
        let mut session = None;
        let registry = create_test_registry();
        let text = r#"{"type":"session.init","provider":"test"}"#;

        let should_continue = handle_text_message(text, &mut sink, &mut session, &registry).await;

        assert!(should_continue);
        assert!(session.is_some());
        assert_eq!(session.as_ref().unwrap().provider_id.0, "test");
    }

    #[tokio::test]
    async fn test_session_init_success_with_custom_voice() {
        let mut sink = MockSink::new();
        let mut session = None;
        let registry = create_test_registry();
        let text = r#"{"type":"session.init","voice":{"id":"custom_voice","speed":1.5}}"#;

        let should_continue = handle_text_message(text, &mut sink, &mut session, &registry).await;

        assert!(should_continue);
        assert!(session.is_some());
        assert_eq!(session.as_ref().unwrap().voice.id, "custom_voice");
        assert!((session.as_ref().unwrap().voice.speed - 1.5).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn test_session_init_error_no_providers() {
        let mut sink = MockSink::new();
        let mut session = None;
        let registry = create_empty_registry();
        let text = r#"{"type":"session.init"}"#;

        let should_continue = handle_text_message(text, &mut sink, &mut session, &registry).await;

        // Should return false (fatal error)
        assert!(!should_continue);
        assert!(session.is_none());
        assert_eq!(sink.messages.len(), 1);

        if let Message::Text(json) = &sink.messages[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            match msg {
                ServerMessage::SessionError { code, .. } => {
                    assert_eq!(code, "no_providers");
                }
                _ => panic!("Expected SessionError, got {:?}", msg),
            }
        } else {
            panic!("Expected text message");
        }
    }

    #[tokio::test]
    async fn test_session_init_error_provider_not_found() {
        let mut sink = MockSink::new();
        let mut session = None;
        let registry = create_test_registry();
        let text = r#"{"type":"session.init","provider":"nonexistent"}"#;

        let should_continue = handle_text_message(text, &mut sink, &mut session, &registry).await;

        // Should return false (fatal error)
        assert!(!should_continue);
        assert!(session.is_none());
        assert_eq!(sink.messages.len(), 1);

        if let Message::Text(json) = &sink.messages[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            match msg {
                ServerMessage::SessionError {
                    code, alternatives, ..
                } => {
                    assert_eq!(code, "provider_unavailable");
                    // Should include available providers as alternatives
                    assert!(alternatives.is_some());
                    let alts = alternatives.unwrap();
                    assert!(alts.contains(&"test".to_string()));
                }
                _ => panic!("Expected SessionError, got {:?}", msg),
            }
        } else {
            panic!("Expected text message");
        }
    }

    #[tokio::test]
    async fn test_session_init_error_provider_unavailable() {
        let mut sink = MockSink::new();
        let mut session = None;
        let mut registry = ProviderRegistry::new();
        registry.register(MockProvider::unavailable(
            "broken",
            "Broken Provider",
            "Connection failed",
        ));
        registry.register(MockProvider::new("working", "Working Provider"));
        registry.set_default(ProviderId::new("broken"));

        let text = r#"{"type":"session.init","provider":"broken"}"#;

        let should_continue = handle_text_message(text, &mut sink, &mut session, &registry).await;

        assert!(!should_continue);
        assert!(session.is_none());

        if let Message::Text(json) = &sink.messages[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            match msg {
                ServerMessage::SessionError {
                    code,
                    message,
                    alternatives,
                } => {
                    assert_eq!(code, "provider_unavailable");
                    assert!(message.contains("Connection failed"));
                    // Should include working provider as alternative
                    let alts = alternatives.unwrap();
                    assert!(alts.contains(&"working".to_string()));
                }
                _ => panic!("Expected SessionError, got {:?}", msg),
            }
        }
    }

    #[tokio::test]
    async fn test_session_init_error_already_initialized() {
        let mut sink = MockSink::new();
        let registry = create_test_registry();

        // First init
        let mut session = None;
        let text = r#"{"type":"session.init"}"#;
        let _ = handle_text_message(text, &mut sink, &mut session, &registry).await;
        assert!(session.is_some());

        // Clear sink for second message
        sink.messages.clear();

        // Second init should fail
        let should_continue = handle_text_message(text, &mut sink, &mut session, &registry).await;

        // Should return true (not fatal, just ignored)
        assert!(should_continue);
        assert_eq!(sink.messages.len(), 1);

        if let Message::Text(json) = &sink.messages[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            match msg {
                ServerMessage::Error { code, .. } => {
                    assert_eq!(code, "session_already_initialized");
                }
                _ => panic!("Expected Error, got {:?}", msg),
            }
        }
    }

    // ==================== Text Message Tests ====================

    #[tokio::test]
    async fn test_text_message_without_session() {
        let mut sink = MockSink::new();
        let mut session = None;
        let registry = create_test_registry();
        let text = r#"{"type":"text","content":"Hello world"}"#;

        let should_continue = handle_text_message(text, &mut sink, &mut session, &registry).await;

        assert!(should_continue);
        assert_eq!(sink.messages.len(), 1);

        if let Message::Text(json) = &sink.messages[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            match msg {
                ServerMessage::Error { code, .. } => {
                    assert_eq!(code, "no_session");
                }
                _ => panic!("Expected Error, got {:?}", msg),
            }
        }
    }

    #[tokio::test]
    async fn test_text_done_without_session() {
        let mut sink = MockSink::new();
        let mut session = None;
        let registry = create_test_registry();
        let text = r#"{"type":"text.done"}"#;

        let should_continue = handle_text_message(text, &mut sink, &mut session, &registry).await;

        assert!(should_continue);
        assert_eq!(sink.messages.len(), 1);

        if let Message::Text(json) = &sink.messages[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            match msg {
                ServerMessage::Error { code, .. } => {
                    assert_eq!(code, "no_session");
                }
                _ => panic!("Expected Error, got {:?}", msg),
            }
        }
    }

    #[tokio::test]
    async fn test_handle_text_message_invalid_json() {
        let mut sink = MockSink::new();
        let mut session = None;
        let registry = create_test_registry();
        let text = "not valid json";

        let should_continue = handle_text_message(text, &mut sink, &mut session, &registry).await;

        assert!(should_continue);
        assert_eq!(sink.messages.len(), 1);

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

    // ==================== Binary Message Tests ====================

    #[tokio::test]
    async fn test_handle_binary_message() {
        let mut sink = MockSink::new();
        let data = vec![0u8; 100];

        let should_continue = handle_binary_message(&data, &mut sink).await;

        assert!(should_continue);
        assert_eq!(sink.messages.len(), 1);

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

    // ==================== Process Message Tests ====================

    #[tokio::test]
    async fn test_process_message_close() {
        let mut sink = MockSink::new();
        let mut session = None;
        let registry = create_test_registry();
        let message = Message::Close(None);

        let should_continue = process_message(message, &mut sink, &mut session, &registry).await;

        assert!(!should_continue);
    }

    #[tokio::test]
    async fn test_process_message_ping() {
        let mut sink = MockSink::new();
        let mut session = None;
        let registry = create_test_registry();
        let message = Message::Ping(vec![1, 2, 3].into());

        let should_continue = process_message(message, &mut sink, &mut session, &registry).await;

        assert!(should_continue);
        // No response expected (Axum handles pong automatically)
        assert!(sink.messages.is_empty());
    }

    // ==================== Utility Tests ====================

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

    // ==================== Session State Integration Tests ====================

    #[tokio::test]
    async fn test_full_session_flow_init_then_text() {
        let mut sink = MockSink::new();
        let mut session = None;
        let registry = create_test_registry();

        // Initialize session
        let init_text = r#"{"type":"session.init"}"#;
        let should_continue =
            handle_text_message(init_text, &mut sink, &mut session, &registry).await;
        assert!(should_continue);
        assert!(session.is_some());

        sink.messages.clear();

        // Send text (should get "not implemented" since text handling is for later)
        let text_msg = r#"{"type":"text","content":"Hello"}"#;
        let should_continue =
            handle_text_message(text_msg, &mut sink, &mut session, &registry).await;
        assert!(should_continue);

        if let Message::Text(json) = &sink.messages[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            // Should be "not_implemented" rather than "no_session"
            if let ServerMessage::Error { code, .. } = msg {
                assert_eq!(code, "not_implemented");
            } else {
                panic!("Expected Error message");
            }
        }
    }
}
