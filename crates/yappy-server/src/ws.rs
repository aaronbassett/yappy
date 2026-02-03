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
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument, warn};
use yappy_core::buffer::Sentence;
use yappy_core::provider::ProviderId;
use yappy_core::{
    AudioFormat, ClientMessage, CodeBlockMode, ServerMessage, Session, SessionState, TtsProvider,
    VoiceConfig,
};

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
#[allow(clippy::too_many_lines)]
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
                ClientMessage::Text { content } => {
                    debug!(message_type = "text", "Received client message");

                    // Check if session is initialized
                    let Some(session) = session.as_mut() else {
                        let response = ServerMessage::error(
                            "no_session",
                            "No active session - send session.init first",
                        );
                        if let Err(err) = send_server_message(sender, &response).await {
                            warn!("Failed to send response: {}", err);
                            return false;
                        }
                        return true;
                    };

                    // Update last activity timestamp
                    session.touch();

                    // Transition to Streaming state on first text message
                    if session.state == SessionState::Ready {
                        session.state = SessionState::Streaming;
                        debug!(
                            session_id = %session.id,
                            "Session transitioned to Streaming state"
                        );
                    }

                    // Push content to sentence buffer and extract complete sentences
                    let sentences = session.buffer.push(&content);

                    // Log sentence count (never log actual text content - privacy requirement)
                    if !sentences.is_empty() {
                        debug!(
                            session_id = %session.id,
                            sentence_count = sentences.len(),
                            "Extracted sentences from buffer"
                        );

                        // Get the provider for synthesis
                        if let Some(provider) = providers.get(&session.provider_id) {
                            // Synthesize sentences and stream audio to WebSocket
                            if !synthesize_and_stream(sentences, session, sender, provider.as_ref())
                                .await
                            {
                                // Fatal error during synthesis/streaming
                                return false;
                            }
                        } else {
                            // Provider disappeared - this shouldn't happen but handle gracefully
                            warn!(
                                session_id = %session.id,
                                provider_id = %session.provider_id,
                                "Provider not found during synthesis"
                            );
                            let response = ServerMessage::error(
                                "provider_gone",
                                "TTS provider is no longer available",
                            );
                            if let Err(err) = send_server_message(sender, &response).await {
                                warn!("Failed to send error: {}", err);
                                return false;
                            }
                        }
                    }

                    true
                }
                ClientMessage::TextDone => {
                    debug!(message_type = "text.done", "Received client message");

                    // Check if session is initialized
                    let Some(session) = session.as_mut() else {
                        let response = ServerMessage::error(
                            "no_session",
                            "No active session - send session.init first",
                        );
                        if let Err(err) = send_server_message(sender, &response).await {
                            warn!("Failed to send response: {}", err);
                            return false;
                        }
                        return true;
                    };

                    // Transition to Completing state
                    session.state = SessionState::Completing;
                    debug!(
                        session_id = %session.id,
                        "Session transitioned to Completing state"
                    );

                    // Flush remaining buffer content and synthesize if any
                    if let Some(sentence) = session.buffer.flush() {
                        // Log that we flushed remaining content (but not the content itself - privacy)
                        debug!(
                            session_id = %session.id,
                            sentence_index = sentence.index,
                            "Flushed remaining buffer content"
                        );

                        // Synthesize the flushed sentence
                        if let Some(provider) = providers.get(&session.provider_id) {
                            if !synthesize_and_stream(
                                vec![sentence],
                                session,
                                sender,
                                provider.as_ref(),
                            )
                            .await
                            {
                                // Fatal error during synthesis
                                return false;
                            }
                        } else {
                            warn!(
                                session_id = %session.id,
                                provider_id = %session.provider_id,
                                "Provider not found during flush synthesis"
                            );
                        }
                    }

                    // Get the total sentence count (includes any flushed sentence)
                    let total_sentences = session.buffer.sentence_index();

                    // Send audio.done with accumulated statistics
                    let response = ServerMessage::audio_done(
                        total_sentences,
                        session.total_duration_ms,
                        session.total_bytes,
                    );
                    if let Err(err) = send_server_message(sender, &response).await {
                        warn!("Failed to send audio.done: {}", err);
                        return false;
                    }

                    // Transition to Closed state
                    session.state = SessionState::Closed;
                    debug!(
                        session_id = %session.id,
                        total_sentences = total_sentences,
                        total_duration_ms = session.total_duration_ms,
                        total_bytes = session.total_bytes,
                        "Session completed and closed"
                    );

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

/// Synthesize sentences and stream audio chunks to the WebSocket.
///
/// For each sentence:
/// 1. Calls the provider's `synthesize()` method to get an audio stream
/// 2. Streams each `AudioChunk` as a binary WebSocket frame
/// 3. Updates session statistics (duration, bytes, sequence)
/// 4. On error, sends a non-fatal error message with the sentence index
///
/// # Arguments
///
/// * `sentences` - Sentences to synthesize (extracted from the buffer)
/// * `session` - Mutable reference to update statistics
/// * `sender` - WebSocket sender for streaming audio frames
/// * `provider` - The TTS provider to use for synthesis
///
/// # Returns
///
/// Returns `true` if all sentences were processed (even if some had errors),
/// `false` if a fatal error occurred (e.g., send failure).
///
/// # Privacy
///
/// Sentence text is never logged. Only metadata like sentence index and
/// chunk sizes are recorded for observability.
async fn synthesize_and_stream<S>(
    sentences: Vec<Sentence>,
    session: &mut Session,
    sender: &mut S,
    provider: &dyn TtsProvider,
) -> bool
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    for sentence in sentences {
        debug!(
            session_id = %session.id,
            sentence_index = sentence.index,
            "Starting synthesis for sentence"
        );

        // Create a cancellation token for this synthesis operation
        // TODO: In the future, wire this to client disconnection or explicit cancel
        let cancel_token = CancellationToken::new();

        // Call the provider to synthesize audio
        let audio_stream = match provider
            .synthesize(
                &sentence.text,
                &session.voice,
                session.audio_format.clone(),
                cancel_token,
            )
            .await
        {
            Ok(stream) => stream,
            Err(err) => {
                // Non-fatal error: send error message with sentence index
                warn!(
                    session_id = %session.id,
                    sentence_index = sentence.index,
                    error_code = err.code(),
                    "Synthesis failed for sentence"
                );

                let response =
                    ServerMessage::error_with_sentence(err.code(), err.to_string(), sentence.index);
                if let Err(send_err) = send_server_message(sender, &response).await {
                    warn!("Failed to send synthesis error: {}", send_err);
                    return false;
                }
                // Continue with next sentence
                continue;
            }
        };

        // Stream audio chunks to WebSocket
        let mut audio_stream = audio_stream;
        while let Some(chunk_result) = audio_stream.next().await {
            match chunk_result {
                Ok(mut chunk) => {
                    // Assign global sequence number from session
                    chunk.sequence =
                        session.record_audio_chunk(chunk.duration_ms, chunk.data.len());

                    // Ensure sentence_index matches our tracked sentence
                    chunk.sentence_index = sentence.index;

                    debug!(
                        session_id = %session.id,
                        sequence = chunk.sequence,
                        sentence_index = chunk.sentence_index,
                        bytes = chunk.data.len(),
                        duration_ms = chunk.duration_ms,
                        "Sending audio chunk"
                    );

                    // Serialize chunk to binary frame and send
                    let frame = chunk.to_binary_frame();
                    if let Err(err) = sender.send(Message::Binary(frame.to_vec().into())).await {
                        warn!("Failed to send audio chunk: {}", err);
                        return false;
                    }
                }
                Err(err) => {
                    // Stream error: send non-fatal error with sentence index
                    warn!(
                        session_id = %session.id,
                        sentence_index = sentence.index,
                        error_code = err.code(),
                        "Audio stream error for sentence"
                    );

                    let response = ServerMessage::error_with_sentence(
                        err.code(),
                        err.to_string(),
                        sentence.index,
                    );
                    if let Err(send_err) = send_server_message(sender, &response).await {
                        warn!("Failed to send stream error: {}", send_err);
                        return false;
                    }
                    // Break out of chunk streaming, continue to next sentence
                    break;
                }
            }
        }

        debug!(
            session_id = %session.id,
            sentence_index = sentence.index,
            "Completed synthesis for sentence"
        );
    }

    true
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
    use futures_util::stream;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio_util::bytes::Bytes;
    use tokio_util::sync::CancellationToken;
    use yappy_core::{
        audio::{AudioChunk, AudioStream},
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

        /// Extract text messages from the sink
        fn text_messages(&self) -> Vec<&str> {
            self.messages
                .iter()
                .filter_map(|m| match m {
                    Message::Text(s) => Some(s.as_str()),
                    _ => None,
                })
                .collect()
        }

        /// Extract binary messages from the sink
        fn binary_messages(&self) -> Vec<&[u8]> {
            self.messages
                .iter()
                .filter_map(|m| match m {
                    Message::Binary(b) => Some(b.as_ref()),
                    _ => None,
                })
                .collect()
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

    /// Mock TTS provider behavior configuration
    #[derive(Clone)]
    enum MockSynthesisMode {
        /// Return a simple audio stream with one chunk per sentence
        Success {
            /// Duration in ms for each chunk
            chunk_duration_ms: u32,
            /// Bytes of audio data per chunk
            chunk_bytes: usize,
        },
        /// Return an error during synthesis initialization
        FailInit { error: String },
        /// Return an error mid-stream (after yielding some chunks)
        FailMidStream {
            chunks_before_error: usize,
            error: String,
        },
    }

    impl Default for MockSynthesisMode {
        fn default() -> Self {
            Self::Success {
                chunk_duration_ms: 100,
                chunk_bytes: 256,
            }
        }
    }

    /// Mock TTS provider for testing
    struct MockProvider {
        id: String,
        name: String,
        status: ProviderStatus,
        voices: Vec<VoiceInfo>,
        synthesis_mode: MockSynthesisMode,
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
                synthesis_mode: MockSynthesisMode::default(),
            }
        }

        fn with_synthesis_mode(mut self, mode: MockSynthesisMode) -> Self {
            self.synthesis_mode = mode;
            self
        }

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
            match &self.synthesis_mode {
                MockSynthesisMode::Success {
                    chunk_duration_ms,
                    chunk_bytes,
                } => {
                    // Return a stream with a single audio chunk
                    let duration = *chunk_duration_ms;
                    let bytes = *chunk_bytes;
                    let chunk = AudioChunk::new(
                        0, // sequence will be assigned by caller
                        0, // sentence_index will be assigned by caller
                        Bytes::from(vec![0xAB; bytes]),
                        duration,
                    );
                    Ok(Box::pin(stream::iter(vec![Ok(chunk)])))
                }
                MockSynthesisMode::FailInit { error } => Err(ProviderError::SynthesisFailed {
                    message: error.clone(),
                }),
                MockSynthesisMode::FailMidStream {
                    chunks_before_error,
                    error,
                } => {
                    let mut items: Vec<Result<AudioChunk, ProviderError>> = Vec::new();
                    for i in 0..*chunks_before_error {
                        items.push(Ok(AudioChunk::new(
                            i as u32,
                            0,
                            Bytes::from(vec![0xAB; 256]),
                            100,
                        )));
                    }
                    items.push(Err(ProviderError::SynthesisFailed {
                        message: error.clone(),
                    }));
                    Ok(Box::pin(stream::iter(items)))
                }
            }
        }
    }

    /// Helper to create a provider registry with a synthesizing mock provider
    fn create_test_registry() -> ProviderRegistry {
        let mut registry = ProviderRegistry::new();
        registry.register(MockProvider::new("test", "Test Provider"));
        registry.set_default(ProviderId::new("test"));
        registry
    }

    /// Helper to create a registry with a provider that has specific synthesis behavior
    fn create_test_registry_with_mode(mode: MockSynthesisMode) -> ProviderRegistry {
        let mut registry = ProviderRegistry::new();
        registry.register(MockProvider::new("test", "Test Provider").with_synthesis_mode(mode));
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
        assert_eq!(session.as_ref().unwrap().state, SessionState::Ready);

        sink.messages.clear();

        // Send text with a complete sentence - should synthesize and send audio
        let text_msg = r#"{"type":"text","content":"Hello world."}"#;
        let should_continue =
            handle_text_message(text_msg, &mut sink, &mut session, &registry).await;
        assert!(should_continue);

        // Now we expect binary audio frames for the complete sentence
        assert_eq!(sink.messages.len(), 1);
        assert!(matches!(sink.messages[0], Message::Binary(_)));

        // Session state should transition to Streaming
        assert_eq!(session.as_ref().unwrap().state, SessionState::Streaming);

        // Session should track accumulated statistics
        let session_ref = session.as_ref().unwrap();
        assert!(session_ref.total_duration_ms > 0);
        assert!(session_ref.total_bytes > 0);
        assert_eq!(session_ref.audio_sequence, 1);
    }

    #[tokio::test]
    async fn test_text_message_extracts_sentences() {
        let mut sink = MockSink::new();
        let mut session = None;
        let registry = create_test_registry();

        // Initialize session
        let init_text = r#"{"type":"session.init"}"#;
        handle_text_message(init_text, &mut sink, &mut session, &registry).await;
        sink.messages.clear();

        // Send text with multiple sentences
        let text_msg = r#"{"type":"text","content":"First sentence. Second sentence. Third"}"#;
        let should_continue =
            handle_text_message(text_msg, &mut sink, &mut session, &registry).await;
        assert!(should_continue);

        // Now we expect 2 binary audio frames (one for each complete sentence)
        assert_eq!(sink.messages.len(), 2);
        assert!(matches!(sink.messages[0], Message::Binary(_)));
        assert!(matches!(sink.messages[1], Message::Binary(_)));

        // Buffer should contain the incomplete part ("Third")
        assert!(!session.as_ref().unwrap().buffer.is_empty());
        assert_eq!(session.as_ref().unwrap().buffer.current_buffer(), "Third");

        // Session should track 2 audio chunks
        assert_eq!(session.as_ref().unwrap().audio_sequence, 2);
    }

    #[tokio::test]
    async fn test_text_message_updates_last_activity() {
        let mut sink = MockSink::new();
        let mut session = None;
        let registry = create_test_registry();

        // Initialize session
        let init_text = r#"{"type":"session.init"}"#;
        handle_text_message(init_text, &mut sink, &mut session, &registry).await;

        let initial_activity = session.as_ref().unwrap().last_activity;

        // Small delay to ensure time difference
        std::thread::sleep(std::time::Duration::from_millis(10));

        sink.messages.clear();

        // Send text (no complete sentence, so no audio)
        let text_msg = r#"{"type":"text","content":"Test"}"#;
        handle_text_message(text_msg, &mut sink, &mut session, &registry).await;

        // last_activity should be updated
        assert!(session.as_ref().unwrap().last_activity > initial_activity);
    }

    #[tokio::test]
    async fn test_text_message_state_stays_streaming_on_subsequent_messages() {
        let mut sink = MockSink::new();
        let mut session = None;
        let registry = create_test_registry();

        // Initialize session
        let init_text = r#"{"type":"session.init"}"#;
        handle_text_message(init_text, &mut sink, &mut session, &registry).await;
        sink.messages.clear();

        // First text message with complete sentence - transitions to Streaming
        let text_msg1 = r#"{"type":"text","content":"First. "}"#;
        handle_text_message(text_msg1, &mut sink, &mut session, &registry).await;
        assert_eq!(session.as_ref().unwrap().state, SessionState::Streaming);

        // Second text message with complete sentence - stays in Streaming
        let text_msg2 = r#"{"type":"text","content":"Second. "}"#;
        handle_text_message(text_msg2, &mut sink, &mut session, &registry).await;
        assert_eq!(session.as_ref().unwrap().state, SessionState::Streaming);

        // Now we expect 2 binary audio frames (one for each complete sentence)
        assert_eq!(sink.messages.len(), 2);
        assert!(matches!(sink.messages[0], Message::Binary(_)));
        assert!(matches!(sink.messages[1], Message::Binary(_)));
    }

    // ==================== Text Done Tests ====================

    #[tokio::test]
    async fn test_text_done_sends_audio_done() {
        let mut sink = MockSink::new();
        let mut session = None;
        let registry = create_test_registry();

        // Initialize session
        let init_text = r#"{"type":"session.init"}"#;
        handle_text_message(init_text, &mut sink, &mut session, &registry).await;
        sink.messages.clear();

        // Send text.done
        let done_text = r#"{"type":"text.done"}"#;
        let should_continue =
            handle_text_message(done_text, &mut sink, &mut session, &registry).await;

        assert!(should_continue);
        assert_eq!(sink.messages.len(), 1);

        // Verify audio.done response
        if let Message::Text(json) = &sink.messages[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            match msg {
                ServerMessage::AudioDone {
                    total_sentences,
                    total_duration_ms,
                    total_bytes,
                } => {
                    assert_eq!(total_sentences, 0); // No sentences sent
                    assert_eq!(total_duration_ms, 0);
                    assert_eq!(total_bytes, 0);
                }
                _ => panic!("Expected AudioDone, got {:?}", msg),
            }
        } else {
            panic!("Expected text message");
        }

        // Session should be in Closed state
        assert_eq!(session.as_ref().unwrap().state, SessionState::Closed);
    }

    #[tokio::test]
    async fn test_text_done_flushes_buffer_and_counts_sentences() {
        let mut sink = MockSink::new();
        let mut session = None;
        let registry = create_test_registry();

        // Initialize session
        let init_text = r#"{"type":"session.init"}"#;
        handle_text_message(init_text, &mut sink, &mut session, &registry).await;
        sink.messages.clear();

        // Send text with complete sentences and an incomplete one
        let text_msg = r#"{"type":"text","content":"First sentence. Second sentence. Incomplete"}"#;
        handle_text_message(text_msg, &mut sink, &mut session, &registry).await;

        // 2 binary audio frames should have been sent for the 2 complete sentences
        assert_eq!(sink.messages.len(), 2);
        assert!(matches!(sink.messages[0], Message::Binary(_)));
        assert!(matches!(sink.messages[1], Message::Binary(_)));

        // Buffer should have the incomplete text
        assert_eq!(
            session.as_ref().unwrap().buffer.current_buffer(),
            "Incomplete"
        );

        sink.messages.clear();

        // Send text.done - should flush the incomplete text and synthesize it
        let done_text = r#"{"type":"text.done"}"#;
        let should_continue =
            handle_text_message(done_text, &mut sink, &mut session, &registry).await;

        assert!(should_continue);
        // Expect 1 binary frame for the flushed sentence + 1 text frame for audio.done
        assert_eq!(sink.messages.len(), 2);
        assert!(matches!(sink.messages[0], Message::Binary(_)));

        // Verify audio.done response has correct sentence count and statistics
        if let Message::Text(json) = &sink.messages[1] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            match msg {
                ServerMessage::AudioDone {
                    total_sentences,
                    total_duration_ms,
                    total_bytes,
                } => {
                    // 2 complete sentences + 1 flushed incomplete = 3 total
                    assert_eq!(total_sentences, 3);
                    // Check statistics are populated (mock returns 100ms and 256 bytes per chunk)
                    assert_eq!(total_duration_ms, 300); // 3 sentences * 100ms
                    assert_eq!(total_bytes, 768); // 3 sentences * 256 bytes
                }
                _ => panic!("Expected AudioDone, got {:?}", msg),
            }
        } else {
            panic!("Expected text message for audio.done");
        }

        // Buffer should be empty after flush
        assert!(session.as_ref().unwrap().buffer.is_empty());
    }

    #[tokio::test]
    async fn test_text_done_state_transitions() {
        let mut sink = MockSink::new();
        let mut session = None;
        let registry = create_test_registry();

        // Initialize session
        let init_text = r#"{"type":"session.init"}"#;
        handle_text_message(init_text, &mut sink, &mut session, &registry).await;
        assert_eq!(session.as_ref().unwrap().state, SessionState::Ready);

        // Send text with complete sentence - transitions to Streaming
        let text_msg = r#"{"type":"text","content":"Hello. "}"#;
        handle_text_message(text_msg, &mut sink, &mut session, &registry).await;
        assert_eq!(session.as_ref().unwrap().state, SessionState::Streaming);

        sink.messages.clear();

        // Send text.done - transitions to Closed (through Completing)
        let done_text = r#"{"type":"text.done"}"#;
        handle_text_message(done_text, &mut sink, &mut session, &registry).await;

        // Final state should be Closed
        assert_eq!(session.as_ref().unwrap().state, SessionState::Closed);
    }

    #[tokio::test]
    async fn test_full_session_flow_init_text_done() {
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

        // Send text with two complete sentences
        let text_msg = r#"{"type":"text","content":"Hello world. Goodbye world."}"#;
        let should_continue =
            handle_text_message(text_msg, &mut sink, &mut session, &registry).await;
        assert!(should_continue);
        // 2 binary frames for the 2 complete sentences
        assert_eq!(sink.messages.len(), 2);
        assert!(matches!(sink.messages[0], Message::Binary(_)));
        assert!(matches!(sink.messages[1], Message::Binary(_)));

        sink.messages.clear();

        // Send text.done
        let done_text = r#"{"type":"text.done"}"#;
        let should_continue =
            handle_text_message(done_text, &mut sink, &mut session, &registry).await;
        assert!(should_continue);

        // Verify audio.done was sent (no flushed content since buffer was empty)
        assert_eq!(sink.messages.len(), 1);
        if let Message::Text(json) = &sink.messages[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            match msg {
                ServerMessage::AudioDone {
                    total_sentences,
                    total_duration_ms,
                    total_bytes,
                } => {
                    assert_eq!(total_sentences, 2);
                    assert_eq!(total_duration_ms, 200); // 2 sentences * 100ms
                    assert_eq!(total_bytes, 512); // 2 sentences * 256 bytes
                }
                _ => panic!("Expected AudioDone, got {:?}", msg),
            }
        } else {
            panic!("Expected text message for audio.done");
        }

        // Session is closed
        assert_eq!(session.as_ref().unwrap().state, SessionState::Closed);
    }

    // ==================== Audio Streaming Tests ====================

    #[tokio::test]
    async fn test_audio_chunk_binary_frame_format() {
        let mut sink = MockSink::new();
        let mut session = None;
        let registry = create_test_registry();

        // Initialize session
        let init_text = r#"{"type":"session.init"}"#;
        handle_text_message(init_text, &mut sink, &mut session, &registry).await;
        sink.messages.clear();

        // Send text with one complete sentence
        let text_msg = r#"{"type":"text","content":"Hello world."}"#;
        handle_text_message(text_msg, &mut sink, &mut session, &registry).await;

        // Should have 1 binary frame
        assert_eq!(sink.messages.len(), 1);

        // Parse the binary frame
        if let Message::Binary(data) = &sink.messages[0] {
            // Frame should have 12-byte header + data
            assert!(data.len() >= 12);

            // Parse header (little-endian u32s)
            let sequence = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            let sentence_index = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
            let duration_ms = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);

            assert_eq!(sequence, 0);
            assert_eq!(sentence_index, 0);
            assert_eq!(duration_ms, 100); // Mock provider returns 100ms

            // Audio data should be mock data (0xAB bytes)
            let audio_data = &data[12..];
            assert_eq!(audio_data.len(), 256); // Mock provider returns 256 bytes
            assert!(audio_data.iter().all(|&b| b == 0xAB));
        } else {
            panic!("Expected binary message");
        }
    }

    #[tokio::test]
    async fn test_synthesis_error_sends_error_message() {
        let mut sink = MockSink::new();
        let mut session = None;
        let registry = create_test_registry_with_mode(MockSynthesisMode::FailInit {
            error: "Test synthesis failure".to_string(),
        });

        // Initialize session
        let init_text = r#"{"type":"session.init"}"#;
        handle_text_message(init_text, &mut sink, &mut session, &registry).await;
        sink.messages.clear();

        // Send text with one complete sentence - synthesis will fail
        let text_msg = r#"{"type":"text","content":"Hello world."}"#;
        let should_continue =
            handle_text_message(text_msg, &mut sink, &mut session, &registry).await;

        // Should continue (non-fatal error)
        assert!(should_continue);

        // Should have 1 error message (no binary audio due to failure)
        assert_eq!(sink.messages.len(), 1);

        if let Message::Text(json) = &sink.messages[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            match msg {
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
                _ => panic!("Expected Error, got {:?}", msg),
            }
        } else {
            panic!("Expected text message");
        }
    }

    #[tokio::test]
    async fn test_multiple_sentences_sequence_numbers() {
        let mut sink = MockSink::new();
        let mut session = None;
        let registry = create_test_registry();

        // Initialize session
        let init_text = r#"{"type":"session.init"}"#;
        handle_text_message(init_text, &mut sink, &mut session, &registry).await;
        sink.messages.clear();

        // Send text with three complete sentences
        let text_msg = r#"{"type":"text","content":"First. Second. Third."}"#;
        handle_text_message(text_msg, &mut sink, &mut session, &registry).await;

        // Should have 3 binary frames
        assert_eq!(sink.messages.len(), 3);

        // Verify sequence numbers and sentence indices
        for (i, msg) in sink.messages.iter().enumerate() {
            if let Message::Binary(data) = msg {
                let sequence = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                let sentence_index = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);

                assert_eq!(sequence, i as u32);
                assert_eq!(sentence_index, i as u32);
            } else {
                panic!("Expected binary message at index {}", i);
            }
        }

        // Session should track 3 audio chunks
        assert_eq!(session.as_ref().unwrap().audio_sequence, 3);
    }
}
