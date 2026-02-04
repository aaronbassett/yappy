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
//! # Session Isolation (FR-018)
//!
//! Each WebSocket connection is fully isolated from other connections:
//!
//! - **Session object**: Created as a stack-local variable in [`handle_socket`], ensuring
//!   complete isolation between connections.
//! - **Sentence buffer**: Owned directly by each [`Session`], not shared. Each session
//!   maintains its own buffer state, sentence indices, and code block parsing state.
//! - **Voice configuration**: Stored per-session in the [`Session`] struct, including
//!   voice ID, speed, pitch, and volume settings.
//! - **Audio encoding**: The [`AudioFormat`] is stored per-session, allowing different
//!   connections to use different codecs (PCM, Opus, MP3).
//! - **Backpressure state**: Each connection has its own [`BackpressureSender`] with
//!   independent semaphore and metrics.
//!
//! The only shared state is:
//! - **Provider registry** (`ProviderRegistry`): Shared read-only via `Arc`. Providers
//!   expose only `&self` methods and are never mutated per-session.
//! - **SRX rules** (sentence segmentation): Static read-only rules initialized once.
//! - **Configuration**: Cloned when creating sessions, so mutations are local.
//!
//! This design ensures no cross-contamination between sessions.
//!
//! # Privacy
//!
//! Per project requirements, text content is never logged. Only metadata such as
//! message types, frame sizes, and connection events are recorded.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument, trace, warn, Span};
use yappy_core::audio::AudioCodec;
use yappy_core::buffer::{BufferConfig, Sentence};
use yappy_core::provider::ProviderId;
use yappy_core::transcode::Transcoder;
use yappy_core::{
    AudioFormat, ClientMessage, CodeBlockMode, ServerMessage, Session, SessionState, TtsProvider,
    VoiceConfig,
};

use crate::shutdown::SessionGuard;
use crate::state::{AppState, ProviderRegistry};

/// Default flush timeout when no session is active yet.
/// This is only used before session.init is received.
const DEFAULT_FLUSH_TIMEOUT: Duration = Duration::from_millis(500);

/// Metrics for tracking backpressure events during a session.
#[derive(Debug, Default)]
struct BackpressureMetrics {
    /// Number of times backpressure was applied (channel was full)
    backpressure_events: AtomicU64,
    /// Total milliseconds spent waiting due to backpressure
    total_backpressure_ms: AtomicU64,
}

impl BackpressureMetrics {
    /// Record a backpressure event with its duration.
    #[allow(clippy::cast_possible_truncation)]
    fn record_backpressure(&self, duration: Duration) {
        self.backpressure_events.fetch_add(1, Ordering::Relaxed);
        // Truncation is acceptable here - backpressure durations won't exceed u64::MAX ms
        self.total_backpressure_ms
            .fetch_add(duration.as_millis() as u64, Ordering::Relaxed);
    }

    /// Get the number of backpressure events.
    fn event_count(&self) -> u64 {
        self.backpressure_events.load(Ordering::Relaxed)
    }

    /// Get the total time spent in backpressure.
    fn total_time_ms(&self) -> u64 {
        self.total_backpressure_ms.load(Ordering::Relaxed)
    }
}

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
/// - Registers with the shutdown coordinator for graceful shutdown
/// - Splits the socket into sender and receiver halves
/// - Tracks session state for the connection
/// - Processes incoming messages (text and binary)
/// - Handles connection close gracefully
/// - Logs connection lifecycle events
///
/// # Arguments
///
/// * `socket` - The upgraded WebSocket connection
/// * `state` - Application state for accessing providers (shared read-only)
///
/// # Session Lifecycle
///
/// Each WebSocket connection can have at most one session. The session is
/// created when the client sends a `session.init` message and remains active
/// until the connection closes.
///
/// # Graceful Shutdown (FR-027, SC-007)
///
/// When the server initiates shutdown:
/// 1. The session's cancellation token is triggered
/// 2. Current synthesis operations are cancelled cooperatively
/// 3. Any remaining buffer content is flushed and synthesized
/// 4. `audio.done` is sent to the client
/// 5. Connection is closed cleanly
///
/// # Session Isolation (FR-018)
///
/// This function creates all per-connection state as local variables, ensuring
/// complete isolation between WebSocket connections:
///
/// - `session: Option<Session>` - Stack-local, unique per connection
/// - `bp_sender: BackpressureSender` - Owns its own semaphore and metrics
/// - `session_guard: SessionGuard` - Tracks this session with shutdown coordinator
///
/// The `state` parameter provides shared read-only access to providers via
/// `Arc<ProviderRegistry>`. Provider methods take `&self` and never mutate
/// state based on session-specific data.
///
/// ## Isolation Guarantees
///
/// - Buffer state: Each session owns its [`SentenceBuffer`] directly
/// - Voice config: Stored in session, not shared
/// - Audio format: Stored in session, not shared
/// - Sequence numbers: Maintained per-session in [`Session::audio_sequence`]
/// - Statistics: Accumulated per-session in [`Session::total_duration_ms`] and [`Session::total_bytes`]
#[allow(clippy::too_many_lines)]
#[instrument(name = "ws_connection", skip_all, fields(remote_addr, session_id = tracing::field::Empty))]
async fn handle_socket(socket: WebSocket, state: AppState) {
    info!("WebSocket connection established");

    // Register with the shutdown coordinator for graceful shutdown tracking (FR-027)
    // The guard automatically unregisters when dropped
    let session_guard = state.shutdown().register_session();

    // Get timeout configurations (FR-016)
    let idle_timeout = state.config().server.idle_timeout();
    let session_init_timeout = state.config().server.session_init_timeout();
    let synthesis_timeout = state.config().server.synthesis_timeout();

    // Get audio channel capacity from config (for backpressure)
    let audio_channel_capacity = state.config().server.audio_channel_capacity;

    // Get max text size from config (FR-008: reasonable size limits on text chunks)
    let max_text_size = state.config().server.max_text_size();

    debug!(
        audio_channel_capacity,
        max_text_size,
        idle_timeout_secs = idle_timeout.as_secs(),
        session_init_timeout_secs = session_init_timeout.as_secs(),
        synthesis_timeout_secs = synthesis_timeout.as_secs(),
        "Configuring WebSocket connection"
    );

    // Split the socket into sender and receiver for independent handling
    let (sender, mut receiver) = socket.split();

    // Wrap sender with backpressure control
    let mut bp_sender = BackpressureSender::new(sender, audio_channel_capacity);

    // Session state for this connection (None until session.init is received)
    let mut session: Option<Session> = None;

    // Track whether we've recorded the session_id to the current span (NFR-005)
    let mut session_id_recorded = false;

    // Track connection start time for session init timeout (T297)
    let connection_start = Instant::now();

    // Track last activity for idle timeout (T295)
    let mut last_activity = Instant::now();

    // Get the shutdown token for monitoring - we hold this for the lifetime of the connection
    let shutdown_token = session_guard.token();

    // Process incoming messages with flush timeout support
    loop {
        // Check if shutdown was requested - if so, initiate graceful close
        if session_guard.is_shutting_down() {
            info!("Server shutdown requested, initiating graceful session close");
            if let Err(e) =
                handle_graceful_shutdown(&mut session, &mut bp_sender, state.providers()).await
            {
                warn!(error = %e, "Error during graceful shutdown");
            }
            break;
        }

        // Check for session init timeout (T297) - only if session is not initialized yet
        if session.is_none() && connection_start.elapsed() > session_init_timeout {
            warn!(
                elapsed_secs = connection_start.elapsed().as_secs(),
                timeout_secs = session_init_timeout.as_secs(),
                "Session init timeout - no session.init received"
            );
            let response = ServerMessage::session_error(
                "session_init_timeout",
                format!(
                    "No session.init message received within {} seconds",
                    session_init_timeout.as_secs()
                ),
            );
            if let Err(err) = send_server_message(bp_sender.inner_mut(), &response).await {
                warn!("Failed to send session init timeout error: {}", err);
            }
            break;
        }

        // Check for idle timeout (T295)
        if last_activity.elapsed() > idle_timeout {
            warn!(
                session_id = session.as_ref().map(|s| s.id.to_string()).as_deref(),
                elapsed_secs = last_activity.elapsed().as_secs(),
                timeout_secs = idle_timeout.as_secs(),
                "Idle connection timeout"
            );
            // Send close frame with reason - the connection will be closed after this
            // Note: We don't send a JSON error here because the WebSocket close frame
            // carries the reason. The client should handle the close frame appropriately.
            if let Some(ref s) = session {
                info!(session_id = %s.id, "Closing connection due to idle timeout");
            }
            break;
        }

        // Determine the flush timeout duration based on session state.
        // If we have a session with content in the buffer, use its configured timeout.
        // Otherwise, use a longer timeout (effectively just waiting for messages).
        let flush_timeout = session
            .as_ref()
            .filter(|s| !s.buffer.is_empty() && s.state == SessionState::Streaming)
            .map_or(DEFAULT_FLUSH_TIMEOUT, |s| s.buffer.flush_timeout());

        // Calculate how long until the next timeout event we need to check
        let time_until_idle_timeout = idle_timeout.saturating_sub(last_activity.elapsed());
        let time_until_init_timeout = if session.is_none() {
            session_init_timeout.saturating_sub(connection_start.elapsed())
        } else {
            Duration::MAX // No init timeout once session is established
        };
        let next_timeout_check = flush_timeout
            .min(time_until_idle_timeout)
            .min(time_until_init_timeout);

        tokio::select! {
            // Check for shutdown signal
            () = shutdown_token.cancelled() => {
                info!("Received shutdown signal, initiating graceful session close");
                if let Err(e) = handle_graceful_shutdown(&mut session, &mut bp_sender, state.providers()).await {
                    warn!(error = %e, "Error during graceful shutdown");
                }
                break;
            }

            // Wait for the next WebSocket message
            result = receiver.next() => {
                match result {
                    Some(Ok(message)) => {
                        // Update last activity timestamp on any message received (T295)
                        last_activity = Instant::now();

                        let buffer_config: BufferConfig = state.config().buffer.clone().into();
                        if !process_message_with_cancellation_and_timeout(
                            message,
                            &mut bp_sender,
                            &mut session,
                            state.providers(),
                            &buffer_config,
                            &session_guard,
                            synthesis_timeout,
                            max_text_size,
                        ).await {
                            // Connection should be closed
                            break;
                        }

                        // Record session_id to the current span once session is initialized (NFR-005)
                        // This ensures all subsequent logs within this connection include session_id
                        if !session_id_recorded {
                            if let Some(ref s) = session {
                                Span::current().record("session_id", s.id.to_string().as_str());
                                session_id_recorded = true;
                            }
                        }
                    }
                    Some(Err(err)) => {
                        // Log connection errors without exposing internal details
                        warn!("WebSocket receive error: {}", err);
                        break;
                    }
                    None => {
                        // Stream ended
                        break;
                    }
                }
            }

            // Periodic timeout check (handles both flush, idle, and init timeouts)
            () = tokio::time::sleep(next_timeout_check) => {
                // Check if the buffer should be flushed due to timeout
                if let Some(ref mut session) = session {
                    if !session.buffer.is_empty()
                        && session.state == SessionState::Streaming
                        && session.buffer.should_timeout_flush()
                    {
                        if let Some(sentence) = session.buffer.flush() {
                            debug!(
                                session_id = %session.id,
                                sentence_index = sentence.index,
                                "Flushed buffer due to timeout (no sentence boundary detected)"
                            );

                            // Synthesize the flushed sentence with cancellation support
                            if let Some(provider) = state.providers().get(&session.provider_id) {
                                if !synthesize_and_stream_with_timeout(
                                    vec![sentence],
                                    session,
                                    &mut bp_sender,
                                    provider.as_ref(),
                                    &session_guard,
                                    synthesis_timeout,
                                )
                                .await
                                {
                                    // Fatal error during synthesis
                                    break;
                                }
                            } else {
                                warn!(
                                    session_id = %session.id,
                                    provider_id = %session.provider_id,
                                    "Provider not found during timeout flush synthesis"
                                );
                            }
                        }
                    }
                }
                // Continue to next iteration - the timeout checks at the start of the loop
                // will handle session init timeout and idle timeout
            }
        }
    }

    // Log backpressure metrics at session close
    let bp_events = bp_sender.metrics().event_count();
    let bp_total_ms = bp_sender.metrics().total_time_ms();

    if let Some(ref s) = session {
        if bp_events > 0 {
            info!(
                session_id = %s.id,
                backpressure_events = bp_events,
                backpressure_total_ms = bp_total_ms,
                "WebSocket connection closed (backpressure was applied)"
            );
        } else {
            info!(session_id = %s.id, "WebSocket connection closed");
        }
    } else {
        info!("WebSocket connection closed (no session)");
    }

    // session_guard is dropped here, unregistering from the shutdown coordinator
}

/// Handle graceful shutdown for an active session.
///
/// This function is called when the server initiates shutdown. It:
/// 1. Flushes any remaining buffer content
/// 2. Sends `audio.done` to the client
/// 3. Transitions the session to Closed state
///
/// # Arguments
///
/// * `session` - The current session (may be None if not initialized)
/// * `sender` - The backpressure sender for sending messages
/// * `providers` - The provider registry
///
/// # Returns
///
/// Returns `Ok(())` if shutdown was handled successfully, or an error string on failure.
async fn handle_graceful_shutdown<S>(
    session: &mut Option<Session>,
    sender: &mut BackpressureSender<S>,
    providers: &ProviderRegistry,
) -> Result<(), String>
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    let Some(session) = session.as_mut() else {
        // No active session, nothing to do
        debug!("No active session during graceful shutdown");
        return Ok(());
    };

    // Skip if already closed
    if session.state == SessionState::Closed {
        return Ok(());
    }

    debug!(
        session_id = %session.id,
        "Performing graceful shutdown for session"
    );

    // Flush any remaining buffer content
    if let Some(sentence) = session.buffer.flush() {
        debug!(
            session_id = %session.id,
            sentence_index = sentence.index,
            "Flushing remaining buffer content during shutdown"
        );

        // Try to synthesize the flushed content (best effort during shutdown)
        if let Some(provider) = providers.get(&session.provider_id) {
            // Use a fresh cancellation token - we want to complete this synthesis
            // even though we're shutting down (graceful completion)
            let cancel_token = CancellationToken::new();

            if let Ok(mut audio_stream) = provider
                .synthesize(
                    &sentence.text,
                    &session.voice,
                    session.audio_format.clone(),
                    cancel_token,
                )
                .await
            {
                // Stream the audio chunks
                while let Some(chunk_result) = audio_stream.next().await {
                    if let Ok(mut chunk) = chunk_result {
                        chunk.sequence =
                            session.record_audio_chunk(chunk.duration_ms, chunk.data.len());
                        chunk.sentence_index = sentence.index;

                        let frame = chunk.to_binary_frame();
                        if sender.send_binary(frame.to_vec()).await.is_err() {
                            // Connection may have closed, break out
                            break;
                        }
                    }
                }
            }
        }
    }

    // Get total statistics
    let total_sentences = session.buffer.sentence_index();

    // Send audio.done
    let response = ServerMessage::audio_done(
        total_sentences,
        session.total_duration_ms,
        session.total_bytes,
    );
    send_server_message(sender.inner_mut(), &response).await?;

    // Mark session as closed
    session.state = SessionState::Closed;

    info!(
        session_id = %session.id,
        total_sentences = total_sentences,
        total_duration_ms = session.total_duration_ms,
        total_bytes = session.total_bytes,
        "Session closed during graceful shutdown"
    );

    Ok(())
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
/// * `sender` - Mutable reference to the backpressure sender for responses
/// * `session` - Mutable reference to the current session state
/// * `providers` - Reference to the provider registry
/// * `buffer_config` - Buffer configuration for new sessions
#[cfg_attr(not(test), allow(dead_code))]
async fn process_message<S>(
    message: Message,
    sender: &mut BackpressureSender<S>,
    session: &mut Option<Session>,
    providers: &ProviderRegistry,
    buffer_config: &BufferConfig,
) -> bool
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    match message {
        Message::Text(text) => {
            handle_text_message(&text, sender, session, providers, buffer_config).await
        }
        Message::Binary(data) => handle_binary_message(&data, sender.inner_mut()).await,
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
#[cfg_attr(not(test), allow(dead_code))]
async fn handle_text_message<S>(
    text: &str,
    sender: &mut BackpressureSender<S>,
    session: &mut Option<Session>,
    providers: &ProviderRegistry,
    buffer_config: &BufferConfig,
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
                        sender.inner_mut(),
                        session,
                        providers,
                        provider,
                        voice,
                        audio_format,
                        code_block_mode,
                        buffer_config,
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
                        if let Err(err) = send_server_message(sender.inner_mut(), &response).await {
                            warn!("Failed to send response: {}", err);
                            return false;
                        }
                        return true;
                    };

                    // Reject text messages when session is completing or closed
                    if session.state == SessionState::Completing
                        || session.state == SessionState::Closed
                    {
                        let response = ServerMessage::error(
                            "session_closed",
                            "Cannot send text after text.done - session is completing or closed",
                        );
                        if let Err(err) = send_server_message(sender.inner_mut(), &response).await {
                            warn!("Failed to send response: {}", err);
                            return false;
                        }
                        return true;
                    }

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
                            // Synthesize sentences and stream audio to WebSocket with backpressure
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
                            if let Err(err) =
                                send_server_message(sender.inner_mut(), &response).await
                            {
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
                        if let Err(err) = send_server_message(sender.inner_mut(), &response).await {
                            warn!("Failed to send response: {}", err);
                            return false;
                        }
                        return true;
                    };

                    // Reject duplicate text.done when session is already completing or closed
                    if session.state == SessionState::Completing
                        || session.state == SessionState::Closed
                    {
                        let response = ServerMessage::error(
                            "duplicate_done",
                            "text.done already received - session is completing or closed",
                        );
                        if let Err(err) = send_server_message(sender.inner_mut(), &response).await {
                            warn!("Failed to send response: {}", err);
                            return false;
                        }
                        return true;
                    }

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
                    if let Err(err) = send_server_message(sender.inner_mut(), &response).await {
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
            if let Err(send_err) = send_server_message(sender.inner_mut(), &response).await {
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
/// 4. Validates voice ID exists in provider's voice list
/// 5. Validates voice parameters (speed, pitch, volume)
/// 6. Resolves audio format
/// 7. Creates the session and sends session.ready
///
/// # Error Handling
///
/// - `session_already_initialized`: If session already exists
/// - `no_providers`: If no providers are registered
/// - `provider_unavailable`: If specified/default provider is not available
/// - `invalid_voice`: If the specified voice ID is not available for the provider
/// - `invalid_voice_config`: If voice parameters are out of valid range
///
/// # Returns
///
/// Returns `true` to continue the message loop, `false` on fatal errors
/// that require closing the connection.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn handle_session_init<S>(
    sender: &mut S,
    session: &mut Option<Session>,
    providers: &ProviderRegistry,
    requested_provider: Option<String>,
    requested_voice: Option<VoiceConfig>,
    requested_format: Option<AudioFormat>,
    requested_code_block_mode: Option<CodeBlockMode>,
    buffer_config: &BufferConfig,
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

    // Get provider metadata for defaults and validation
    let metadata = provider.metadata();

    // Collect available voice IDs for validation
    let available_voice_ids: Vec<String> = metadata.voices.iter().map(|v| v.id.clone()).collect();

    // Resolve voice configuration (use default if not specified)
    let voice = if let Some(requested) = requested_voice {
        // Validate voice ID exists in provider's voice list
        if !available_voice_ids.is_empty() && !available_voice_ids.contains(&requested.id) {
            let response = ServerMessage::session_error_with_alternatives(
                "invalid_voice",
                format!(
                    "Voice '{}' is not available for provider '{}'",
                    requested.id, provider_id
                ),
                available_voice_ids,
            );
            if let Err(err) = send_server_message(sender, &response).await {
                warn!("Failed to send response: {err}");
            }
            return false;
        }

        // Validate voice parameters (speed, pitch, volume)
        if let Err(validation_error) = requested.validate() {
            let response = ServerMessage::session_error(
                "invalid_voice_config",
                format!("Invalid voice configuration: {validation_error}"),
            );
            if let Err(err) = send_server_message(sender, &response).await {
                warn!("Failed to send response: {err}");
            }
            return false;
        }

        requested
    } else {
        // Use default voice if available
        let default_voice_id = metadata
            .voices
            .first()
            .map(|v| v.id.clone())
            .unwrap_or_default();
        VoiceConfig {
            id: default_voice_id,
            ..Default::default()
        }
    };

    // Resolve and validate audio format
    let audio_format = if let Some(requested) = requested_format {
        // Client requested a specific format - validate it
        let Some(resolved) = negotiate_audio_format(&metadata.supported_formats, &requested) else {
            // Format cannot be provided (not native and can't transcode)
            let available_codecs: Vec<String> = metadata
                .supported_formats
                .iter()
                .map(|f| f.codec.to_string())
                .collect();
            let response = ServerMessage::session_error(
                "invalid_format",
                format!(
                    "Audio format '{}' is not available for provider '{}'. Available: {}. \
                     Transcoding from PCM is supported for: opus, mp3.",
                    requested.codec,
                    provider_id,
                    available_codecs.join(", ")
                ),
            );
            if let Err(err) = send_server_message(sender, &response).await {
                warn!("Failed to send response: {err}");
            }
            return false;
        };
        resolved
    } else {
        // Use provider's default format (first in supported_formats)
        metadata
            .supported_formats
            .first()
            .cloned()
            .unwrap_or_default()
    };

    // Resolve code block mode
    let code_block_mode = requested_code_block_mode.unwrap_or_default();

    // Create the session with the configured buffer settings.
    //
    // ISOLATION NOTE (FR-018): This creates a completely new Session instance
    // with all owned state. The Session is stored in the stack-local `session`
    // variable in handle_socket(), ensuring no sharing between connections.
    // - buffer_config is cloned, so mutations are local
    // - Session owns its SentenceBuffer directly
    // - All statistics start at zero for this session
    let new_session = Session::with_buffer_config(
        provider_id,
        voice.clone(),
        audio_format.clone(),
        code_block_mode,
        buffer_config.clone(),
    );
    let session_id = new_session.id.clone();

    // Debug assertion: verify the session starts with clean state (FR-018 isolation)
    debug_assert!(
        new_session.buffer.is_empty(),
        "New session should have empty buffer"
    );
    debug_assert_eq!(
        new_session.audio_sequence, 0,
        "New session should start at sequence 0"
    );
    debug_assert_eq!(
        new_session.total_bytes, 0,
        "New session should have zero bytes"
    );
    debug_assert_eq!(
        new_session.total_duration_ms, 0,
        "New session should have zero duration"
    );

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
///
/// # Backpressure
///
/// This function implements FR-020 backpressure. When the bounded audio channel
/// is full (client is slow consuming audio), the synthesis will pause until
/// the channel has capacity. This prevents unbounded memory growth.
#[cfg_attr(not(test), allow(dead_code))]
async fn synthesize_and_stream<S>(
    sentences: Vec<Sentence>,
    session: &mut Session,
    sender: &mut BackpressureSender<S>,
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
                if let Err(send_err) = send_server_message(sender.inner_mut(), &response).await {
                    warn!("Failed to send synthesis error: {}", send_err);
                    return false;
                }
                // Continue with next sentence
                continue;
            }
        };

        // Stream audio chunks to WebSocket with backpressure control
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

                    // Serialize chunk to binary frame and send with backpressure
                    // This will block if the channel is at capacity (FR-020)
                    let frame = chunk.to_binary_frame();
                    if let Err(err) = sender.send_binary(frame.to_vec()).await {
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
                    if let Err(send_err) = send_server_message(sender.inner_mut(), &response).await
                    {
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

/// Process a single WebSocket message with cancellation support.
///
/// This is a wrapper around message handling that passes the session guard
/// for cancellation token propagation to synthesis operations.
///
/// # Arguments
///
/// * `message` - The received WebSocket message
/// * `sender` - Mutable reference to the backpressure sender for responses
/// * `session` - Mutable reference to the current session state
/// * `providers` - Reference to the provider registry
/// * `buffer_config` - Buffer configuration for new sessions
/// * `session_guard` - Session guard for accessing cancellation tokens
#[allow(dead_code)]
async fn process_message_with_cancellation<S>(
    message: Message,
    sender: &mut BackpressureSender<S>,
    session: &mut Option<Session>,
    providers: &ProviderRegistry,
    buffer_config: &BufferConfig,
    session_guard: &SessionGuard,
) -> bool
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    match message {
        Message::Text(text) => {
            handle_text_message_with_cancellation(
                &text,
                sender,
                session,
                providers,
                buffer_config,
                session_guard,
            )
            .await
        }
        Message::Binary(data) => handle_binary_message(&data, sender.inner_mut()).await,
        Message::Ping(_) => {
            debug!("Received ping");
            true
        }
        Message::Pong(_) => {
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

/// Handle a text WebSocket frame with cancellation support.
///
/// Similar to `handle_text_message` but propagates the session guard
/// to synthesis operations for graceful shutdown support.
#[allow(clippy::too_many_lines, dead_code)]
async fn handle_text_message_with_cancellation<S>(
    text: &str,
    sender: &mut BackpressureSender<S>,
    session: &mut Option<Session>,
    providers: &ProviderRegistry,
    buffer_config: &BufferConfig,
    session_guard: &SessionGuard,
) -> bool
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    match serde_json::from_str::<ClientMessage>(text) {
        Ok(client_msg) => match client_msg {
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
                    sender.inner_mut(),
                    session,
                    providers,
                    provider,
                    voice,
                    audio_format,
                    code_block_mode,
                    buffer_config,
                )
                .await
            }
            ClientMessage::Text { content } => {
                debug!(message_type = "text", "Received client message");

                let Some(session) = session.as_mut() else {
                    let response = ServerMessage::error(
                        "no_session",
                        "No active session - send session.init first",
                    );
                    if let Err(err) = send_server_message(sender.inner_mut(), &response).await {
                        warn!("Failed to send response: {}", err);
                        return false;
                    }
                    return true;
                };

                // Reject text messages when session is completing or closed
                if session.state == SessionState::Completing
                    || session.state == SessionState::Closed
                {
                    let response = ServerMessage::error(
                        "session_closed",
                        "Cannot send text after text.done - session is completing or closed",
                    );
                    if let Err(err) = send_server_message(sender.inner_mut(), &response).await {
                        warn!("Failed to send response: {}", err);
                        return false;
                    }
                    return true;
                }

                session.touch();

                if session.state == SessionState::Ready {
                    session.state = SessionState::Streaming;
                    debug!(
                        session_id = %session.id,
                        "Session transitioned to Streaming state"
                    );
                }

                let sentences = session.buffer.push(&content);

                if !sentences.is_empty() {
                    debug!(
                        session_id = %session.id,
                        sentence_count = sentences.len(),
                        "Extracted sentences from buffer"
                    );

                    if let Some(provider) = providers.get(&session.provider_id) {
                        if !synthesize_and_stream_with_cancellation(
                            sentences,
                            session,
                            sender,
                            provider.as_ref(),
                            session_guard,
                        )
                        .await
                        {
                            return false;
                        }
                    } else {
                        warn!(
                            session_id = %session.id,
                            provider_id = %session.provider_id,
                            "Provider not found during synthesis"
                        );
                        let response = ServerMessage::error(
                            "provider_gone",
                            "TTS provider is no longer available",
                        );
                        if let Err(err) = send_server_message(sender.inner_mut(), &response).await {
                            warn!("Failed to send error: {}", err);
                            return false;
                        }
                    }
                }

                true
            }
            ClientMessage::TextDone => {
                debug!(message_type = "text.done", "Received client message");

                let Some(session) = session.as_mut() else {
                    let response = ServerMessage::error(
                        "no_session",
                        "No active session - send session.init first",
                    );
                    if let Err(err) = send_server_message(sender.inner_mut(), &response).await {
                        warn!("Failed to send response: {}", err);
                        return false;
                    }
                    return true;
                };

                // Reject duplicate text.done when session is already completing or closed
                if session.state == SessionState::Completing
                    || session.state == SessionState::Closed
                {
                    let response = ServerMessage::error(
                        "duplicate_done",
                        "text.done already received - session is completing or closed",
                    );
                    if let Err(err) = send_server_message(sender.inner_mut(), &response).await {
                        warn!("Failed to send response: {}", err);
                        return false;
                    }
                    return true;
                }

                session.state = SessionState::Completing;
                debug!(
                    session_id = %session.id,
                    "Session transitioned to Completing state"
                );

                if let Some(sentence) = session.buffer.flush() {
                    debug!(
                        session_id = %session.id,
                        sentence_index = sentence.index,
                        "Flushed remaining buffer content"
                    );

                    if let Some(provider) = providers.get(&session.provider_id) {
                        if !synthesize_and_stream_with_cancellation(
                            vec![sentence],
                            session,
                            sender,
                            provider.as_ref(),
                            session_guard,
                        )
                        .await
                        {
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

                let total_sentences = session.buffer.sentence_index();

                let response = ServerMessage::audio_done(
                    total_sentences,
                    session.total_duration_ms,
                    session.total_bytes,
                );
                if let Err(err) = send_server_message(sender.inner_mut(), &response).await {
                    warn!("Failed to send audio.done: {}", err);
                    return false;
                }

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
        },
        Err(err) => {
            warn!(error = %err, "Failed to parse client message");

            let response =
                ServerMessage::error("invalid_message", "Failed to parse message as valid JSON");
            if let Err(send_err) = send_server_message(sender.inner_mut(), &response).await {
                warn!("Failed to send error response: {}", send_err);
                return false;
            }
            true
        }
    }
}

/// Synthesize sentences with cancellation token propagation (FR-027).
///
/// Similar to `synthesize_and_stream` but uses the session guard's cancellation
/// token to support graceful shutdown. When shutdown is initiated, ongoing
/// synthesis operations receive a cancellation signal and should stop within
/// 5 seconds (per FR-027).
///
/// # Arguments
///
/// * `sentences` - Sentences to synthesize
/// * `session` - Mutable reference to update statistics
/// * `sender` - WebSocket sender for streaming audio frames
/// * `provider` - The TTS provider to use for synthesis
/// * `session_guard` - Session guard providing cancellation tokens
///
/// # Returns
///
/// Returns `true` if all sentences were processed, `false` on fatal error.
#[allow(clippy::too_many_lines, dead_code)]
async fn synthesize_and_stream_with_cancellation<S>(
    sentences: Vec<Sentence>,
    session: &mut Session,
    sender: &mut BackpressureSender<S>,
    provider: &dyn TtsProvider,
    session_guard: &SessionGuard,
) -> bool
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    for sentence in sentences {
        // Check for shutdown before starting each sentence
        if session_guard.is_shutting_down() {
            debug!(
                session_id = %session.id,
                sentence_index = sentence.index,
                "Skipping sentence synthesis due to shutdown"
            );
            return true; // Return true to allow graceful shutdown handler to run
        }

        debug!(
            session_id = %session.id,
            sentence_index = sentence.index,
            "Starting synthesis for sentence"
        );

        // Create a child cancellation token that will be cancelled on shutdown
        let cancel_token = session_guard.child_token();

        let audio_stream = match provider
            .synthesize(
                &sentence.text,
                &session.voice,
                session.audio_format.clone(),
                cancel_token.clone(),
            )
            .await
        {
            Ok(stream) => stream,
            Err(err) => {
                // Check if this was due to cancellation
                if cancel_token.is_cancelled() {
                    debug!(
                        session_id = %session.id,
                        sentence_index = sentence.index,
                        "Synthesis cancelled due to shutdown"
                    );
                    return true;
                }

                warn!(
                    session_id = %session.id,
                    sentence_index = sentence.index,
                    error_code = err.code(),
                    "Synthesis failed for sentence"
                );

                let response =
                    ServerMessage::error_with_sentence(err.code(), err.to_string(), sentence.index);
                if let Err(send_err) = send_server_message(sender.inner_mut(), &response).await {
                    warn!("Failed to send synthesis error: {}", send_err);
                    return false;
                }
                continue;
            }
        };

        let mut audio_stream = audio_stream;
        while let Some(chunk_result) = audio_stream.next().await {
            // Check for cancellation during streaming
            if cancel_token.is_cancelled() {
                debug!(
                    session_id = %session.id,
                    sentence_index = sentence.index,
                    "Audio streaming cancelled due to shutdown"
                );
                return true;
            }

            match chunk_result {
                Ok(mut chunk) => {
                    chunk.sequence =
                        session.record_audio_chunk(chunk.duration_ms, chunk.data.len());
                    chunk.sentence_index = sentence.index;

                    debug!(
                        session_id = %session.id,
                        sequence = chunk.sequence,
                        sentence_index = chunk.sentence_index,
                        bytes = chunk.data.len(),
                        duration_ms = chunk.duration_ms,
                        "Sending audio chunk"
                    );

                    let frame = chunk.to_binary_frame();
                    if let Err(err) = sender.send_binary(frame.to_vec()).await {
                        warn!("Failed to send audio chunk: {}", err);
                        return false;
                    }
                }
                Err(err) => {
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
                    if let Err(send_err) = send_server_message(sender.inner_mut(), &response).await
                    {
                        warn!("Failed to send stream error: {}", send_err);
                        return false;
                    }
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

/// Synthesize sentences with timeout support (FR-016, T296).
///
/// This function wraps synthesis operations with a configurable timeout. If synthesis
/// for a single sentence exceeds the timeout, a `synthesis_failed` error with `timeout`
/// reason is sent and the sentence is skipped.
///
/// # Arguments
///
/// * `sentences` - Sentences to synthesize
/// * `session` - Mutable reference to update statistics
/// * `sender` - WebSocket sender for streaming audio frames
/// * `provider` - The TTS provider to use for synthesis
/// * `session_guard` - Session guard providing cancellation tokens
/// * `timeout` - Maximum time allowed for synthesizing each sentence
///
/// # Returns
///
/// Returns `true` if all sentences were processed, `false` on fatal error.
#[allow(clippy::too_many_lines)]
async fn synthesize_and_stream_with_timeout<S>(
    sentences: Vec<Sentence>,
    session: &mut Session,
    sender: &mut BackpressureSender<S>,
    provider: &dyn TtsProvider,
    session_guard: &SessionGuard,
    timeout: Duration,
) -> bool
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    for sentence in sentences {
        // Check for shutdown before starting each sentence
        if session_guard.is_shutting_down() {
            debug!(
                session_id = %session.id,
                sentence_index = sentence.index,
                "Skipping sentence synthesis due to shutdown"
            );
            return true;
        }

        debug!(
            session_id = %session.id,
            sentence_index = sentence.index,
            timeout_secs = timeout.as_secs(),
            "Starting synthesis for sentence with timeout"
        );

        // Create a child cancellation token that will be cancelled on shutdown
        let cancel_token = session_guard.child_token();

        // Wrap the synthesis call with a timeout (T296)
        let synthesis_result = tokio::time::timeout(
            timeout,
            provider.synthesize(
                &sentence.text,
                &session.voice,
                session.audio_format.clone(),
                cancel_token.clone(),
            ),
        )
        .await;

        let audio_stream = match synthesis_result {
            Ok(Ok(stream)) => stream,
            Ok(Err(err)) => {
                // Synthesis failed (not due to timeout)
                if cancel_token.is_cancelled() {
                    debug!(
                        session_id = %session.id,
                        sentence_index = sentence.index,
                        "Synthesis cancelled due to shutdown"
                    );
                    return true;
                }

                warn!(
                    session_id = %session.id,
                    sentence_index = sentence.index,
                    error_code = err.code(),
                    "Synthesis failed for sentence"
                );

                let response =
                    ServerMessage::error_with_sentence(err.code(), err.to_string(), sentence.index);
                if let Err(send_err) = send_server_message(sender.inner_mut(), &response).await {
                    warn!("Failed to send synthesis error: {}", send_err);
                    return false;
                }
                continue;
            }
            Err(_elapsed) => {
                // Synthesis timed out (T296)
                warn!(
                    session_id = %session.id,
                    sentence_index = sentence.index,
                    timeout_secs = timeout.as_secs(),
                    "Synthesis timeout for sentence"
                );

                let response = ServerMessage::error_with_sentence(
                    "synthesis_failed",
                    format!("Synthesis timed out after {} seconds", timeout.as_secs()),
                    sentence.index,
                );
                if let Err(send_err) = send_server_message(sender.inner_mut(), &response).await {
                    warn!("Failed to send synthesis timeout error: {}", send_err);
                    return false;
                }
                continue;
            }
        };

        // Stream audio chunks with per-chunk timeout monitoring
        let mut audio_stream = audio_stream;
        let chunk_start = Instant::now();

        while let Some(chunk_result) = audio_stream.next().await {
            // Check for cancellation during streaming
            if cancel_token.is_cancelled() {
                debug!(
                    session_id = %session.id,
                    sentence_index = sentence.index,
                    "Audio streaming cancelled due to shutdown"
                );
                return true;
            }

            // Check if streaming has exceeded the timeout (for long audio streams)
            if chunk_start.elapsed() > timeout {
                warn!(
                    session_id = %session.id,
                    sentence_index = sentence.index,
                    elapsed_secs = chunk_start.elapsed().as_secs(),
                    "Audio streaming timeout for sentence"
                );

                let response = ServerMessage::error_with_sentence(
                    "synthesis_failed",
                    format!(
                        "Audio streaming timed out after {} seconds",
                        chunk_start.elapsed().as_secs()
                    ),
                    sentence.index,
                );
                if let Err(send_err) = send_server_message(sender.inner_mut(), &response).await {
                    warn!("Failed to send streaming timeout error: {}", send_err);
                    return false;
                }
                break;
            }

            match chunk_result {
                Ok(mut chunk) => {
                    chunk.sequence =
                        session.record_audio_chunk(chunk.duration_ms, chunk.data.len());
                    chunk.sentence_index = sentence.index;

                    debug!(
                        session_id = %session.id,
                        sequence = chunk.sequence,
                        sentence_index = chunk.sentence_index,
                        bytes = chunk.data.len(),
                        duration_ms = chunk.duration_ms,
                        "Sending audio chunk"
                    );

                    let frame = chunk.to_binary_frame();
                    if let Err(err) = sender.send_binary(frame.to_vec()).await {
                        warn!("Failed to send audio chunk: {}", err);
                        return false;
                    }
                }
                Err(err) => {
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
                    if let Err(send_err) = send_server_message(sender.inner_mut(), &response).await
                    {
                        warn!("Failed to send stream error: {}", send_err);
                        return false;
                    }
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

/// Process a single WebSocket message with cancellation and timeout support.
///
/// This is a wrapper around message handling that passes the session guard
/// for cancellation token propagation and applies synthesis timeout.
///
/// # Arguments
///
/// * `message` - The received WebSocket message
/// * `sender` - Mutable reference to the backpressure sender for responses
/// * `session` - Mutable reference to the current session state
/// * `providers` - Reference to the provider registry
/// * `buffer_config` - Buffer configuration for new sessions
/// * `session_guard` - Session guard for accessing cancellation tokens
/// * `synthesis_timeout` - Maximum time for synthesis operations
/// * `max_text_size` - Maximum allowed text message size in bytes (FR-008)
#[allow(clippy::too_many_arguments)]
async fn process_message_with_cancellation_and_timeout<S>(
    message: Message,
    sender: &mut BackpressureSender<S>,
    session: &mut Option<Session>,
    providers: &ProviderRegistry,
    buffer_config: &BufferConfig,
    session_guard: &SessionGuard,
    synthesis_timeout: Duration,
    max_text_size: usize,
) -> bool
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    match message {
        Message::Text(text) => {
            // Validate text message size (FR-008: reasonable size limits on text chunks)
            if text.len() > max_text_size {
                warn!(
                    message_size = text.len(),
                    max_size = max_text_size,
                    "Text message exceeds maximum allowed size"
                );
                let response = ServerMessage::error(
                    "message_too_large",
                    format!(
                        "Text message size ({} bytes) exceeds maximum allowed size ({} bytes)",
                        text.len(),
                        max_text_size
                    ),
                );
                if let Err(err) = send_server_message(sender.inner_mut(), &response).await {
                    warn!("Failed to send size limit error: {}", err);
                    return false;
                }
                // Return true to continue the message loop - this is a non-fatal error
                return true;
            }

            handle_text_message_with_timeout(
                &text,
                sender,
                session,
                providers,
                buffer_config,
                session_guard,
                synthesis_timeout,
            )
            .await
        }
        Message::Binary(data) => handle_binary_message(&data, sender.inner_mut()).await,
        Message::Ping(_) => {
            debug!("Received ping");
            true
        }
        Message::Pong(_) => {
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

/// Handle a text WebSocket frame with timeout support.
///
/// Similar to `handle_text_message_with_cancellation` but applies synthesis timeout
/// to all synthesis operations.
#[allow(clippy::too_many_lines)]
async fn handle_text_message_with_timeout<S>(
    text: &str,
    sender: &mut BackpressureSender<S>,
    session: &mut Option<Session>,
    providers: &ProviderRegistry,
    buffer_config: &BufferConfig,
    session_guard: &SessionGuard,
    synthesis_timeout: Duration,
) -> bool
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    match serde_json::from_str::<ClientMessage>(text) {
        Ok(client_msg) => match client_msg {
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
                    sender.inner_mut(),
                    session,
                    providers,
                    provider,
                    voice,
                    audio_format,
                    code_block_mode,
                    buffer_config,
                )
                .await
            }
            ClientMessage::Text { content } => {
                debug!(message_type = "text", "Received client message");

                let Some(session) = session.as_mut() else {
                    let response = ServerMessage::error(
                        "no_session",
                        "No active session - send session.init first",
                    );
                    if let Err(err) = send_server_message(sender.inner_mut(), &response).await {
                        warn!("Failed to send response: {}", err);
                        return false;
                    }
                    return true;
                };

                // Reject text messages when session is completing or closed
                if session.state == SessionState::Completing
                    || session.state == SessionState::Closed
                {
                    let response = ServerMessage::error(
                        "session_closed",
                        "Cannot send text after text.done - session is completing or closed",
                    );
                    if let Err(err) = send_server_message(sender.inner_mut(), &response).await {
                        warn!("Failed to send response: {}", err);
                        return false;
                    }
                    return true;
                }

                session.touch();

                if session.state == SessionState::Ready {
                    session.state = SessionState::Streaming;
                    debug!(
                        session_id = %session.id,
                        "Session transitioned to Streaming state"
                    );
                }

                let sentences = session.buffer.push(&content);

                if !sentences.is_empty() {
                    debug!(
                        session_id = %session.id,
                        sentence_count = sentences.len(),
                        "Extracted sentences from buffer"
                    );

                    if let Some(provider) = providers.get(&session.provider_id) {
                        if !synthesize_and_stream_with_timeout(
                            sentences,
                            session,
                            sender,
                            provider.as_ref(),
                            session_guard,
                            synthesis_timeout,
                        )
                        .await
                        {
                            return false;
                        }
                    } else {
                        warn!(
                            session_id = %session.id,
                            provider_id = %session.provider_id,
                            "Provider not found during synthesis"
                        );
                        let response = ServerMessage::error(
                            "provider_gone",
                            "TTS provider is no longer available",
                        );
                        if let Err(err) = send_server_message(sender.inner_mut(), &response).await {
                            warn!("Failed to send error: {}", err);
                            return false;
                        }
                    }
                }

                true
            }
            ClientMessage::TextDone => {
                debug!(message_type = "text.done", "Received client message");

                let Some(session) = session.as_mut() else {
                    let response = ServerMessage::error(
                        "no_session",
                        "No active session - send session.init first",
                    );
                    if let Err(err) = send_server_message(sender.inner_mut(), &response).await {
                        warn!("Failed to send response: {}", err);
                        return false;
                    }
                    return true;
                };

                // Reject duplicate text.done when session is already completing or closed
                if session.state == SessionState::Completing
                    || session.state == SessionState::Closed
                {
                    let response = ServerMessage::error(
                        "duplicate_done",
                        "text.done already received - session is completing or closed",
                    );
                    if let Err(err) = send_server_message(sender.inner_mut(), &response).await {
                        warn!("Failed to send response: {}", err);
                        return false;
                    }
                    return true;
                }

                session.state = SessionState::Completing;
                debug!(
                    session_id = %session.id,
                    "Session transitioned to Completing state"
                );

                if let Some(sentence) = session.buffer.flush() {
                    debug!(
                        session_id = %session.id,
                        sentence_index = sentence.index,
                        "Flushed remaining buffer content"
                    );

                    if let Some(provider) = providers.get(&session.provider_id) {
                        if !synthesize_and_stream_with_timeout(
                            vec![sentence],
                            session,
                            sender,
                            provider.as_ref(),
                            session_guard,
                            synthesis_timeout,
                        )
                        .await
                        {
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

                let total_sentences = session.buffer.sentence_index();

                let response = ServerMessage::audio_done(
                    total_sentences,
                    session.total_duration_ms,
                    session.total_bytes,
                );
                if let Err(err) = send_server_message(sender.inner_mut(), &response).await {
                    warn!("Failed to send audio.done: {}", err);
                    return false;
                }

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
        },
        Err(err) => {
            warn!(error = %err, "Failed to parse client message");

            let response =
                ServerMessage::error("invalid_message", "Failed to parse message as valid JSON");
            if let Err(send_err) = send_server_message(sender.inner_mut(), &response).await {
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

/// A wrapper around a WebSocket sender that implements bounded backpressure.
///
/// This struct uses a semaphore to limit the number of in-flight frames.
/// When the limit is reached, sends will block until a permit is released,
/// naturally applying backpressure to the synthesis pipeline.
///
/// # Backpressure Mechanism
///
/// The backpressure works as follows:
/// 1. Before sending a frame, acquire a permit from the semaphore
/// 2. If no permits are available, the acquire will block (backpressure applied)
/// 3. After the frame is sent successfully, the permit is released
///
/// This ensures that at most `capacity` frames are "in flight" between
/// synthesis and the client consuming them.
struct BackpressureSender<S> {
    /// The underlying sender (WebSocket sink)
    sender: S,
    /// Semaphore to limit in-flight frames
    semaphore: Arc<tokio::sync::Semaphore>,
    /// Metrics for tracking backpressure events
    metrics: Arc<BackpressureMetrics>,
    /// Session ID for logging (optional, for context)
    session_id: Option<String>,
    /// Channel capacity for logging
    capacity: usize,
}

impl<S> BackpressureSender<S>
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    /// Create a new backpressure sender with the given capacity.
    ///
    /// # Arguments
    ///
    /// * `sender` - The underlying WebSocket sender
    /// * `capacity` - Maximum number of in-flight frames (32-64 recommended)
    fn new(sender: S, capacity: usize) -> Self {
        Self {
            sender,
            semaphore: Arc::new(tokio::sync::Semaphore::new(capacity)),
            metrics: Arc::new(BackpressureMetrics::default()),
            session_id: None,
            capacity,
        }
    }

    /// Set the session ID for logging context.
    #[allow(dead_code)]
    fn with_session_id(mut self, session_id: String) -> Self {
        self.session_id = Some(session_id);
        self
    }

    /// Get the backpressure metrics.
    fn metrics(&self) -> &BackpressureMetrics {
        &self.metrics
    }

    /// Send a binary frame with backpressure control.
    ///
    /// This method will block if the channel is at capacity, applying
    /// backpressure to the synthesis pipeline.
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` if the frame was sent successfully, or an error string
    /// if sending failed.
    #[allow(clippy::cast_possible_truncation)]
    async fn send_binary(&mut self, data: Vec<u8>) -> Result<(), String> {
        let start = Instant::now();

        // Try to acquire a permit immediately - use if let with else for the backpressure path
        let permit = if let Ok(permit) = self.semaphore.clone().try_acquire_owned() {
            permit
        } else {
            // Channel is at capacity - log and wait for permit (backpressure)
            trace!(
                session_id = self.session_id.as_deref().unwrap_or("unknown"),
                capacity = self.capacity,
                "Backpressure: channel at capacity, waiting for permit"
            );

            // Block until a permit is available
            match self.semaphore.clone().acquire_owned().await {
                Ok(permit) => {
                    let wait_duration = start.elapsed();
                    self.metrics.record_backpressure(wait_duration);

                    // Truncation is acceptable - wait times won't exceed u64::MAX ms
                    debug!(
                        session_id = self.session_id.as_deref().unwrap_or("unknown"),
                        wait_ms = wait_duration.as_millis() as u64,
                        total_backpressure_events = self.metrics.event_count(),
                        "Backpressure: resumed after waiting"
                    );
                    permit
                }
                Err(e) => {
                    return Err(format!("Semaphore closed: {e}"));
                }
            }
        };

        // Send the binary frame
        let result = self
            .sender
            .send(Message::Binary(data.into()))
            .await
            .map_err(|e| format!("Send failed: {e}"));

        // Release the permit after sending (even on error, to avoid deadlock)
        drop(permit);

        result
    }

    /// Send a text (JSON) message.
    ///
    /// Control messages bypass the backpressure mechanism to ensure
    /// timely delivery of error messages and session state updates.
    #[allow(dead_code)]
    async fn send_text(&mut self, json: String) -> Result<(), String> {
        self.sender
            .send(Message::Text(json.into()))
            .await
            .map_err(|e| format!("Send failed: {e}"))
    }

    /// Get a mutable reference to the underlying sender.
    ///
    /// This is used for operations that need direct access to the sender,
    /// such as sending messages before the session is initialized.
    fn inner_mut(&mut self) -> &mut S {
        &mut self.sender
    }
}

/// Negotiate audio format based on provider capabilities and transcoding.
///
/// This function determines if the requested audio format can be provided,
/// either natively by the provider or via transcoding from PCM.
///
/// # Arguments
///
/// * `supported_formats` - Formats natively supported by the provider
/// * `requested` - The format requested by the client
///
/// # Returns
///
/// Returns `Some(AudioFormat)` if the format can be provided:
/// - The requested format if natively supported
/// - The requested format if the provider supports PCM and transcoding is available
///
/// Returns `None` if the format cannot be provided.
fn negotiate_audio_format(
    supported_formats: &[AudioFormat],
    requested: &AudioFormat,
) -> Option<AudioFormat> {
    // Check if the exact codec is natively supported
    let native_support = supported_formats.iter().any(|f| f.codec == requested.codec);

    if native_support {
        // Provider natively supports this codec - use the requested format
        return Some(requested.clone());
    }

    // Check if we can transcode from PCM to the requested format
    let provider_supports_pcm = supported_formats.iter().any(|f| f.codec == AudioCodec::Pcm);

    if provider_supports_pcm && Transcoder::can_transcode(AudioCodec::Pcm, requested.codec) {
        // We can transcode from PCM to the requested format
        // Return the requested format (transcoding will happen at stream time)
        return Some(requested.clone());
    }

    // Format cannot be provided
    None
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
        #[allow(dead_code)]
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
        #[allow(dead_code)]
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

    /// Default capacity for backpressure in tests
    const TEST_BACKPRESSURE_CAPACITY: usize = 32;

    /// Wrapper for `BackpressureSender` that provides convenient access to messages for testing.
    struct TestSender {
        bp_sender: BackpressureSender<MockSink>,
    }

    impl TestSender {
        fn new() -> Self {
            Self {
                bp_sender: BackpressureSender::new(MockSink::new(), TEST_BACKPRESSURE_CAPACITY),
            }
        }

        /// Get a mutable reference to the backpressure sender for passing to handler functions.
        fn as_bp_sender(&mut self) -> &mut BackpressureSender<MockSink> {
            &mut self.bp_sender
        }

        /// Get access to the messages sent.
        fn messages(&self) -> &Vec<Message> {
            &self.bp_sender.sender.messages
        }

        /// Clear the messages for subsequent test steps.
        fn clear_messages(&mut self) {
            self.bp_sender.sender.messages.clear();
        }

        /// Get backpressure metrics for assertions.
        #[allow(dead_code)]
        fn backpressure_events(&self) -> u64 {
            self.bp_sender.metrics().event_count()
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
        #[allow(dead_code)]
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

        fn with_voices(mut self, voices: Vec<VoiceInfo>) -> Self {
            self.voices = voices;
            self
        }
    }

    #[async_trait]
    #[allow(clippy::cast_possible_truncation)]
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

    /// Helper to create a provider registry with multiple voices for voice validation tests
    fn create_test_registry_with_voices() -> ProviderRegistry {
        let mut registry = ProviderRegistry::new();
        let voices = vec![
            VoiceInfo {
                id: "voice_alice".to_string(),
                name: "Alice".to_string(),
                language: "en-US".to_string(),
                gender: None,
                sample_url: None,
            },
            VoiceInfo {
                id: "voice_bob".to_string(),
                name: "Bob".to_string(),
                language: "en-US".to_string(),
                gender: None,
                sample_url: None,
            },
            VoiceInfo {
                id: "voice_carol".to_string(),
                name: "Carol".to_string(),
                language: "en-GB".to_string(),
                gender: None,
                sample_url: None,
            },
        ];
        registry.register(MockProvider::new("test", "Test Provider").with_voices(voices));
        registry.set_default(ProviderId::new("test"));
        registry
    }

    /// Helper to create default buffer config for tests
    fn default_buffer_config() -> BufferConfig {
        BufferConfig::default()
    }

    // ==================== Session Init Tests ====================

    #[tokio::test]
    async fn test_session_init_success_with_default_provider() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry();
        let text = r#"{"type":"session.init"}"#;

        let should_continue = handle_text_message(
            text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        assert!(should_continue);
        assert!(session.is_some());
        assert_eq!(sender.messages().len(), 1);

        // Verify it's a session.ready response
        if let Message::Text(json) = &sender.messages()[0] {
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
                _ => panic!("Expected SessionReady, got {msg:?}"),
            }
        } else {
            panic!("Expected text message");
        }
    }

    #[tokio::test]
    async fn test_session_init_success_with_specified_provider() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry();
        let text = r#"{"type":"session.init","provider":"test"}"#;

        let should_continue = handle_text_message(
            text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        assert!(should_continue);
        assert!(session.is_some());
        assert_eq!(session.as_ref().unwrap().provider_id.0, "test");
    }

    #[tokio::test]
    async fn test_session_init_success_with_valid_voice() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry_with_voices();
        let text = r#"{"type":"session.init","voice":{"id":"voice_alice","speed":1.5}}"#;

        let should_continue = handle_text_message(
            text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        assert!(should_continue);
        assert!(session.is_some());
        assert_eq!(session.as_ref().unwrap().voice.id, "voice_alice");
        assert!((session.as_ref().unwrap().voice.speed - 1.5).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn test_session_init_success_with_voice_parameters_at_bounds() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry_with_voices();
        // Test with voice parameters at valid boundary values
        let text = r#"{"type":"session.init","voice":{"id":"voice_bob","speed":2.0,"pitch":1.0,"volume":0.0}}"#;

        let should_continue = handle_text_message(
            text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        assert!(should_continue);
        assert!(session.is_some());
        let voice = &session.as_ref().unwrap().voice;
        assert_eq!(voice.id, "voice_bob");
        assert!((voice.speed - 2.0).abs() < f32::EPSILON);
        assert!((voice.pitch - 1.0).abs() < f32::EPSILON);
        assert!(voice.volume.abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn test_session_init_error_no_providers() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_empty_registry();
        let text = r#"{"type":"session.init"}"#;

        let should_continue = handle_text_message(
            text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        // Should return false (fatal error)
        assert!(!should_continue);
        assert!(session.is_none());
        assert_eq!(sender.messages().len(), 1);

        if let Message::Text(json) = &sender.messages()[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            match msg {
                ServerMessage::SessionError { code, .. } => {
                    assert_eq!(code, "no_providers");
                }
                _ => panic!("Expected SessionError, got {msg:?}"),
            }
        } else {
            panic!("Expected text message");
        }
    }

    #[tokio::test]
    async fn test_session_init_error_provider_not_found() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry();
        let text = r#"{"type":"session.init","provider":"nonexistent"}"#;

        let should_continue = handle_text_message(
            text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        // Should return false (fatal error)
        assert!(!should_continue);
        assert!(session.is_none());
        assert_eq!(sender.messages().len(), 1);

        if let Message::Text(json) = &sender.messages()[0] {
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
                _ => panic!("Expected SessionError, got {msg:?}"),
            }
        } else {
            panic!("Expected text message");
        }
    }

    #[tokio::test]
    async fn test_session_init_error_provider_unavailable() {
        let mut sender = TestSender::new();
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

        let should_continue = handle_text_message(
            text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        assert!(!should_continue);
        assert!(session.is_none());

        if let Message::Text(json) = &sender.messages()[0] {
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
                _ => panic!("Expected SessionError, got {msg:?}"),
            }
        }
    }

    #[tokio::test]
    async fn test_session_init_error_already_initialized() {
        let mut sender = TestSender::new();
        let registry = create_test_registry();

        // First init
        let mut session = None;
        let text = r#"{"type":"session.init"}"#;
        let _ = handle_text_message(
            text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;
        assert!(session.is_some());

        // Clear sink for second message
        sender.clear_messages();

        // Second init should fail
        let should_continue = handle_text_message(
            text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        // Should return true (not fatal, just ignored)
        assert!(should_continue);
        assert_eq!(sender.messages().len(), 1);

        if let Message::Text(json) = &sender.messages()[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            match msg {
                ServerMessage::Error { code, .. } => {
                    assert_eq!(code, "session_already_initialized");
                }
                _ => panic!("Expected Error, got {msg:?}"),
            }
        }
    }

    // ==================== Voice Validation Tests ====================

    #[tokio::test]
    async fn test_session_init_error_invalid_voice() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry_with_voices();
        let text = r#"{"type":"session.init","voice":{"id":"nonexistent_voice"}}"#;

        let should_continue = handle_text_message(
            text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        // Should return false (fatal error)
        assert!(!should_continue);
        assert!(session.is_none());
        assert_eq!(sender.messages().len(), 1);

        if let Message::Text(json) = &sender.messages()[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            match msg {
                ServerMessage::SessionError {
                    code,
                    message,
                    alternatives,
                } => {
                    assert_eq!(code, "invalid_voice");
                    assert!(message.contains("nonexistent_voice"));
                    assert!(message.contains("not available"));
                    // Should include available voices as alternatives
                    let alts = alternatives.expect("alternatives should be present");
                    assert!(alts.contains(&"voice_alice".to_string()));
                    assert!(alts.contains(&"voice_bob".to_string()));
                    assert!(alts.contains(&"voice_carol".to_string()));
                }
                _ => panic!("Expected SessionError, got {msg:?}"),
            }
        } else {
            panic!("Expected text message");
        }
    }

    #[tokio::test]
    async fn test_session_init_error_invalid_voice_speed_too_low() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry_with_voices();
        // Speed below valid range (0.5 - 2.0)
        let text = r#"{"type":"session.init","voice":{"id":"voice_alice","speed":0.3}}"#;

        let should_continue = handle_text_message(
            text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        // Should return false (fatal error)
        assert!(!should_continue);
        assert!(session.is_none());
        assert_eq!(sender.messages().len(), 1);

        if let Message::Text(json) = &sender.messages()[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            match msg {
                ServerMessage::SessionError { code, message, .. } => {
                    assert_eq!(code, "invalid_voice_config");
                    assert!(message.contains("speed"));
                }
                _ => panic!("Expected SessionError, got {msg:?}"),
            }
        } else {
            panic!("Expected text message");
        }
    }

    #[tokio::test]
    async fn test_session_init_error_invalid_voice_speed_too_high() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry_with_voices();
        // Speed above valid range (0.5 - 2.0)
        let text = r#"{"type":"session.init","voice":{"id":"voice_alice","speed":3.0}}"#;

        let should_continue = handle_text_message(
            text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        // Should return false (fatal error)
        assert!(!should_continue);
        assert!(session.is_none());
        assert_eq!(sender.messages().len(), 1);

        if let Message::Text(json) = &sender.messages()[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            match msg {
                ServerMessage::SessionError { code, message, .. } => {
                    assert_eq!(code, "invalid_voice_config");
                    assert!(message.contains("speed"));
                }
                _ => panic!("Expected SessionError, got {msg:?}"),
            }
        } else {
            panic!("Expected text message");
        }
    }

    #[tokio::test]
    async fn test_session_init_error_invalid_voice_pitch_out_of_range() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry_with_voices();
        // Pitch outside valid range (-1.0 to 1.0)
        let text = r#"{"type":"session.init","voice":{"id":"voice_alice","pitch":1.5}}"#;

        let should_continue = handle_text_message(
            text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        // Should return false (fatal error)
        assert!(!should_continue);
        assert!(session.is_none());
        assert_eq!(sender.messages().len(), 1);

        if let Message::Text(json) = &sender.messages()[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            match msg {
                ServerMessage::SessionError { code, message, .. } => {
                    assert_eq!(code, "invalid_voice_config");
                    assert!(message.contains("pitch"));
                }
                _ => panic!("Expected SessionError, got {msg:?}"),
            }
        } else {
            panic!("Expected text message");
        }
    }

    #[tokio::test]
    async fn test_session_init_error_invalid_voice_volume_out_of_range() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry_with_voices();
        // Volume outside valid range (0.0 to 1.0)
        let text = r#"{"type":"session.init","voice":{"id":"voice_alice","volume":1.5}}"#;

        let should_continue = handle_text_message(
            text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        // Should return false (fatal error)
        assert!(!should_continue);
        assert!(session.is_none());
        assert_eq!(sender.messages().len(), 1);

        if let Message::Text(json) = &sender.messages()[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            match msg {
                ServerMessage::SessionError { code, message, .. } => {
                    assert_eq!(code, "invalid_voice_config");
                    assert!(message.contains("volume"));
                }
                _ => panic!("Expected SessionError, got {msg:?}"),
            }
        } else {
            panic!("Expected text message");
        }
    }

    #[tokio::test]
    async fn test_session_init_error_invalid_voice_negative_volume() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry_with_voices();
        // Negative volume is invalid
        let text = r#"{"type":"session.init","voice":{"id":"voice_alice","volume":-0.5}}"#;

        let should_continue = handle_text_message(
            text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        // Should return false (fatal error)
        assert!(!should_continue);
        assert!(session.is_none());
        assert_eq!(sender.messages().len(), 1);

        if let Message::Text(json) = &sender.messages()[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            match msg {
                ServerMessage::SessionError { code, message, .. } => {
                    assert_eq!(code, "invalid_voice_config");
                    assert!(message.contains("volume"));
                }
                _ => panic!("Expected SessionError, got {msg:?}"),
            }
        } else {
            panic!("Expected text message");
        }
    }

    #[tokio::test]
    async fn test_session_init_uses_first_voice_as_default() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry_with_voices();
        // No voice specified - should use first voice
        let text = r#"{"type":"session.init"}"#;

        let should_continue = handle_text_message(
            text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        assert!(should_continue);
        assert!(session.is_some());
        // Should use the first voice as default
        assert_eq!(session.as_ref().unwrap().voice.id, "voice_alice");

        if let Message::Text(json) = &sender.messages()[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            match msg {
                ServerMessage::SessionReady { voice, .. } => {
                    assert_eq!(voice, "voice_alice");
                }
                _ => panic!("Expected SessionReady, got {msg:?}"),
            }
        } else {
            panic!("Expected text message");
        }
    }

    #[tokio::test]
    async fn test_session_init_voice_validation_uses_correct_provider() {
        // Test that voice validation uses the provider's voice list, not a global list
        let mut sender = TestSender::new();
        let mut session = None;

        // Create a registry with two providers having different voice lists
        let mut registry = ProviderRegistry::new();
        let provider1_voices = vec![VoiceInfo {
            id: "provider1_voice".to_string(),
            name: "Provider 1 Voice".to_string(),
            language: "en-US".to_string(),
            gender: None,
            sample_url: None,
        }];
        let provider2_voices = vec![VoiceInfo {
            id: "provider2_voice".to_string(),
            name: "Provider 2 Voice".to_string(),
            language: "en-US".to_string(),
            gender: None,
            sample_url: None,
        }];
        registry
            .register(MockProvider::new("provider1", "Provider 1").with_voices(provider1_voices));
        registry
            .register(MockProvider::new("provider2", "Provider 2").with_voices(provider2_voices));
        registry.set_default(ProviderId::new("provider1"));

        // Try to use provider1's voice with provider2 - should fail
        let text =
            r#"{"type":"session.init","provider":"provider2","voice":{"id":"provider1_voice"}}"#;
        let should_continue = handle_text_message(
            text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        // Should return false (fatal error)
        assert!(!should_continue);
        assert!(session.is_none());

        if let Message::Text(json) = &sender.messages()[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            match msg {
                ServerMessage::SessionError {
                    code, alternatives, ..
                } => {
                    assert_eq!(code, "invalid_voice");
                    // Should include provider2's voices as alternatives
                    let alts = alternatives.expect("alternatives should be present");
                    assert!(alts.contains(&"provider2_voice".to_string()));
                    assert!(!alts.contains(&"provider1_voice".to_string()));
                }
                _ => panic!("Expected SessionError, got {msg:?}"),
            }
        }
    }

    // ==================== Format Negotiation Tests ====================

    #[test]
    fn test_negotiate_audio_format_native_support() {
        // Provider natively supports Opus
        let formats = vec![AudioFormat {
            codec: yappy_core::AudioCodec::Opus,
            sample_rate: 48000,
            channels: 1,
            bits_per_sample: None,
        }];

        let requested = AudioFormat {
            codec: yappy_core::AudioCodec::Opus,
            sample_rate: 48000,
            channels: 1,
            bits_per_sample: None,
        };

        let result = negotiate_audio_format(&formats, &requested);
        assert!(result.is_some());
        assert_eq!(result.unwrap().codec, yappy_core::AudioCodec::Opus);
    }

    #[test]
    fn test_negotiate_audio_format_transcode_from_pcm() {
        // Provider only supports PCM, client wants Opus
        let formats = vec![AudioFormat {
            codec: yappy_core::AudioCodec::Pcm,
            sample_rate: 24000,
            channels: 1,
            bits_per_sample: Some(16),
        }];

        let requested = AudioFormat {
            codec: yappy_core::AudioCodec::Opus,
            sample_rate: 24000,
            channels: 1,
            bits_per_sample: None,
        };

        let result = negotiate_audio_format(&formats, &requested);
        assert!(result.is_some());
        assert_eq!(result.unwrap().codec, yappy_core::AudioCodec::Opus);
    }

    #[test]
    fn test_negotiate_audio_format_transcode_to_mp3() {
        // Provider only supports PCM, client wants MP3
        let formats = vec![AudioFormat {
            codec: yappy_core::AudioCodec::Pcm,
            sample_rate: 24000,
            channels: 1,
            bits_per_sample: Some(16),
        }];

        let requested = AudioFormat {
            codec: yappy_core::AudioCodec::Mp3,
            sample_rate: 24000,
            channels: 1,
            bits_per_sample: None,
        };

        let result = negotiate_audio_format(&formats, &requested);
        assert!(result.is_some());
        assert_eq!(result.unwrap().codec, yappy_core::AudioCodec::Mp3);
    }

    #[test]
    fn test_negotiate_audio_format_unsupported() {
        // Provider only supports Opus (no PCM), client wants MP3 - can't transcode
        let formats = vec![AudioFormat {
            codec: yappy_core::AudioCodec::Opus,
            sample_rate: 48000,
            channels: 1,
            bits_per_sample: None,
        }];

        let requested = AudioFormat {
            codec: yappy_core::AudioCodec::Mp3,
            sample_rate: 48000,
            channels: 1,
            bits_per_sample: None,
        };

        let result = negotiate_audio_format(&formats, &requested);
        assert!(result.is_none());
    }

    /// Helper to create a provider registry with specific supported formats
    #[allow(clippy::items_after_statements)]
    fn create_test_registry_with_formats(formats: Vec<AudioFormat>) -> ProviderRegistry {
        let mut registry = ProviderRegistry::new();

        // Create a mock provider with specific formats
        struct FormatProvider {
            formats: Vec<AudioFormat>,
        }

        #[async_trait]
        impl TtsProvider for FormatProvider {
            fn metadata(&self) -> yappy_core::provider::ProviderMetadata {
                yappy_core::provider::ProviderMetadata {
                    id: ProviderId::new("format_test"),
                    name: "Format Test Provider".to_string(),
                    description: "Provider for testing format negotiation".to_string(),
                    voices: vec![yappy_core::provider::VoiceInfo {
                        id: "test_voice".to_string(),
                        name: "Test Voice".to_string(),
                        language: "en-US".to_string(),
                        gender: None,
                        sample_url: None,
                    }],
                    supported_formats: self.formats.clone(),
                    options_schema: None,
                }
            }

            async fn health_check(&self) -> yappy_core::ProviderStatus {
                yappy_core::ProviderStatus::Available
            }

            async fn synthesize(
                &self,
                _text: &str,
                _voice: &VoiceConfig,
                _format: AudioFormat,
                _cancel: CancellationToken,
            ) -> Result<yappy_core::audio::AudioStream, yappy_core::error::ProviderError>
            {
                // Return an empty stream
                Ok(Box::pin(stream::empty()))
            }
        }

        registry.register(FormatProvider { formats });
        registry.set_default(ProviderId::new("format_test"));
        registry
    }

    #[tokio::test]
    async fn test_session_init_with_valid_format() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry_with_formats(vec![
            AudioFormat {
                codec: yappy_core::AudioCodec::Opus,
                sample_rate: 48000,
                channels: 1,
                bits_per_sample: None,
            },
            AudioFormat {
                codec: yappy_core::AudioCodec::Pcm,
                sample_rate: 24000,
                channels: 1,
                bits_per_sample: Some(16),
            },
        ]);

        // Request Opus which is natively supported
        let text = r#"{"type":"session.init","audio_format":{"codec":"opus","sample_rate":48000,"channels":1}}"#;

        let should_continue = handle_text_message(
            text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        assert!(should_continue);
        assert!(session.is_some());
        assert_eq!(
            session.as_ref().unwrap().audio_format.codec,
            yappy_core::AudioCodec::Opus
        );
    }

    #[tokio::test]
    async fn test_session_init_with_transcode_format() {
        let mut sender = TestSender::new();
        let mut session = None;
        // Provider only supports PCM
        let registry = create_test_registry_with_formats(vec![AudioFormat {
            codec: yappy_core::AudioCodec::Pcm,
            sample_rate: 24000,
            channels: 1,
            bits_per_sample: Some(16),
        }]);

        // Request MP3 which can be transcoded from PCM
        let text = r#"{"type":"session.init","audio_format":{"codec":"mp3","sample_rate":24000,"channels":1}}"#;

        let should_continue = handle_text_message(
            text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        assert!(should_continue);
        assert!(session.is_some());
        assert_eq!(
            session.as_ref().unwrap().audio_format.codec,
            yappy_core::AudioCodec::Mp3
        );
    }

    #[tokio::test]
    async fn test_session_init_error_invalid_format() {
        let mut sender = TestSender::new();
        let mut session = None;
        // Provider only supports Opus (no PCM, so can't transcode to MP3)
        let registry = create_test_registry_with_formats(vec![AudioFormat {
            codec: yappy_core::AudioCodec::Opus,
            sample_rate: 48000,
            channels: 1,
            bits_per_sample: None,
        }]);

        // Request MP3 which can't be provided
        let text = r#"{"type":"session.init","audio_format":{"codec":"mp3","sample_rate":48000,"channels":1}}"#;

        let should_continue = handle_text_message(
            text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        // Should return false (fatal error)
        assert!(!should_continue);
        assert!(session.is_none());
        assert_eq!(sender.messages().len(), 1);

        if let Message::Text(json) = &sender.messages()[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            match msg {
                ServerMessage::SessionError { code, message, .. } => {
                    assert_eq!(code, "invalid_format");
                    assert!(message.contains("mp3"));
                    assert!(message.contains("not available"));
                }
                _ => panic!("Expected SessionError, got {msg:?}"),
            }
        } else {
            panic!("Expected text message");
        }
    }

    #[tokio::test]
    async fn test_session_init_default_format_when_not_specified() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry_with_formats(vec![
            AudioFormat {
                codec: yappy_core::AudioCodec::Pcm,
                sample_rate: 24000,
                channels: 1,
                bits_per_sample: Some(16),
            },
            AudioFormat {
                codec: yappy_core::AudioCodec::Opus,
                sample_rate: 48000,
                channels: 1,
                bits_per_sample: None,
            },
        ]);

        // No format specified - should use first one (PCM)
        let text = r#"{"type":"session.init"}"#;

        let should_continue = handle_text_message(
            text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        assert!(should_continue);
        assert!(session.is_some());
        // Should use the first format (PCM) as default
        assert_eq!(
            session.as_ref().unwrap().audio_format.codec,
            yappy_core::AudioCodec::Pcm
        );
    }

    // ==================== Text Message Tests ====================

    #[tokio::test]
    async fn test_text_message_without_session() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry();
        let text = r#"{"type":"text","content":"Hello world"}"#;

        let should_continue = handle_text_message(
            text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        assert!(should_continue);
        assert_eq!(sender.messages().len(), 1);

        if let Message::Text(json) = &sender.messages()[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            match msg {
                ServerMessage::Error { code, .. } => {
                    assert_eq!(code, "no_session");
                }
                _ => panic!("Expected Error, got {msg:?}"),
            }
        }
    }

    #[tokio::test]
    async fn test_text_done_without_session() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry();
        let text = r#"{"type":"text.done"}"#;

        let should_continue = handle_text_message(
            text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        assert!(should_continue);
        assert_eq!(sender.messages().len(), 1);

        if let Message::Text(json) = &sender.messages()[0] {
            let msg: ServerMessage = serde_json::from_str(json).unwrap();
            match msg {
                ServerMessage::Error { code, .. } => {
                    assert_eq!(code, "no_session");
                }
                _ => panic!("Expected Error, got {msg:?}"),
            }
        }
    }

    #[tokio::test]
    async fn test_handle_text_message_invalid_json() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry();
        let text = "not valid json";

        let should_continue = handle_text_message(
            text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        assert!(should_continue);
        assert_eq!(sender.messages().len(), 1);

        if let Message::Text(json) = &sender.messages()[0] {
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
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry();
        let message = Message::Close(None);

        let should_continue = process_message(
            message,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        assert!(!should_continue);
    }

    #[tokio::test]
    async fn test_process_message_ping() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry();
        let message = Message::Ping(vec![1, 2, 3].into());

        let should_continue = process_message(
            message,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        assert!(should_continue);
        // No response expected (Axum handles pong automatically)
        assert!(sender.messages().is_empty());
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
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry();

        // Initialize session
        let init_text = r#"{"type":"session.init"}"#;
        let should_continue = handle_text_message(
            init_text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;
        assert!(should_continue);
        assert!(session.is_some());
        assert_eq!(session.as_ref().unwrap().state, SessionState::Ready);

        sender.clear_messages();

        // Send text with a complete sentence followed by more text (streaming style).
        // SRX-based sentence detection only emits sentences when followed by more text,
        // which is correct for streaming TTS (we need to know the sentence is complete).
        let text_msg = r#"{"type":"text","content":"Hello world. And more"}"#;
        let should_continue = handle_text_message(
            text_msg,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;
        assert!(should_continue);

        // Now we expect binary audio frames for the complete sentence
        assert_eq!(sender.messages().len(), 1);
        assert!(matches!(sender.messages()[0], Message::Binary(_)));

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
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry();

        // Initialize session
        let init_text = r#"{"type":"session.init"}"#;
        handle_text_message(
            init_text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;
        sender.clear_messages();

        // Send text with multiple sentences
        let text_msg = r#"{"type":"text","content":"First sentence. Second sentence. Third"}"#;
        let should_continue = handle_text_message(
            text_msg,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;
        assert!(should_continue);

        // Now we expect 2 binary audio frames (one for each complete sentence)
        assert_eq!(sender.messages().len(), 2);
        assert!(matches!(sender.messages()[0], Message::Binary(_)));
        assert!(matches!(sender.messages()[1], Message::Binary(_)));

        // Buffer should contain the incomplete part ("Third")
        assert!(!session.as_ref().unwrap().buffer.is_empty());
        assert_eq!(session.as_ref().unwrap().buffer.current_buffer(), "Third");

        // Session should track 2 audio chunks
        assert_eq!(session.as_ref().unwrap().audio_sequence, 2);
    }

    #[tokio::test]
    async fn test_text_message_updates_last_activity() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry();

        // Initialize session
        let init_text = r#"{"type":"session.init"}"#;
        handle_text_message(
            init_text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        let initial_activity = session.as_ref().unwrap().last_activity;

        // Small delay to ensure time difference
        std::thread::sleep(std::time::Duration::from_millis(10));

        sender.clear_messages();

        // Send text (no complete sentence, so no audio)
        let text_msg = r#"{"type":"text","content":"Test"}"#;
        handle_text_message(
            text_msg,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        // last_activity should be updated
        assert!(session.as_ref().unwrap().last_activity > initial_activity);
    }

    #[tokio::test]
    async fn test_text_message_state_stays_streaming_on_subsequent_messages() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry();

        // Initialize session
        let init_text = r#"{"type":"session.init"}"#;
        handle_text_message(
            init_text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;
        sender.clear_messages();

        // First text message with complete sentence - transitions to Streaming
        let text_msg1 = r#"{"type":"text","content":"First. "}"#;
        handle_text_message(
            text_msg1,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;
        assert_eq!(session.as_ref().unwrap().state, SessionState::Streaming);

        // Second text message with complete sentence - stays in Streaming
        let text_msg2 = r#"{"type":"text","content":"Second. "}"#;
        handle_text_message(
            text_msg2,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;
        assert_eq!(session.as_ref().unwrap().state, SessionState::Streaming);

        // Now we expect 2 binary audio frames (one for each complete sentence)
        assert_eq!(sender.messages().len(), 2);
        assert!(matches!(sender.messages()[0], Message::Binary(_)));
        assert!(matches!(sender.messages()[1], Message::Binary(_)));
    }

    // ==================== Text Done Tests ====================

    #[tokio::test]
    async fn test_text_done_sends_audio_done() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry();

        // Initialize session
        let init_text = r#"{"type":"session.init"}"#;
        handle_text_message(
            init_text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;
        sender.clear_messages();

        // Send text.done
        let done_text = r#"{"type":"text.done"}"#;
        let should_continue = handle_text_message(
            done_text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        assert!(should_continue);
        assert_eq!(sender.messages().len(), 1);

        // Verify audio.done response
        if let Message::Text(json) = &sender.messages()[0] {
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
                _ => panic!("Expected AudioDone, got {msg:?}"),
            }
        } else {
            panic!("Expected text message");
        }

        // Session should be in Closed state
        assert_eq!(session.as_ref().unwrap().state, SessionState::Closed);
    }

    #[tokio::test]
    async fn test_text_done_flushes_buffer_and_counts_sentences() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry();

        // Initialize session
        let init_text = r#"{"type":"session.init"}"#;
        handle_text_message(
            init_text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;
        sender.clear_messages();

        // Send text with complete sentences and an incomplete one
        let text_msg = r#"{"type":"text","content":"First sentence. Second sentence. Incomplete"}"#;
        handle_text_message(
            text_msg,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        // 2 binary audio frames should have been sent for the 2 complete sentences
        assert_eq!(sender.messages().len(), 2);
        assert!(matches!(sender.messages()[0], Message::Binary(_)));
        assert!(matches!(sender.messages()[1], Message::Binary(_)));

        // Buffer should have the incomplete text
        assert_eq!(
            session.as_ref().unwrap().buffer.current_buffer(),
            "Incomplete"
        );

        sender.clear_messages();

        // Send text.done - should flush the incomplete text and synthesize it
        let done_text = r#"{"type":"text.done"}"#;
        let should_continue = handle_text_message(
            done_text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        assert!(should_continue);
        // Expect 1 binary frame for the flushed sentence + 1 text frame for audio.done
        assert_eq!(sender.messages().len(), 2);
        assert!(matches!(sender.messages()[0], Message::Binary(_)));

        // Verify audio.done response has correct sentence count and statistics
        if let Message::Text(json) = &sender.messages()[1] {
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
                _ => panic!("Expected AudioDone, got {msg:?}"),
            }
        } else {
            panic!("Expected text message for audio.done");
        }

        // Buffer should be empty after flush
        assert!(session.as_ref().unwrap().buffer.is_empty());
    }

    #[tokio::test]
    async fn test_text_done_state_transitions() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry();

        // Initialize session
        let init_text = r#"{"type":"session.init"}"#;
        handle_text_message(
            init_text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;
        assert_eq!(session.as_ref().unwrap().state, SessionState::Ready);

        // Send text with complete sentence - transitions to Streaming
        let text_msg = r#"{"type":"text","content":"Hello. "}"#;
        handle_text_message(
            text_msg,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;
        assert_eq!(session.as_ref().unwrap().state, SessionState::Streaming);

        sender.clear_messages();

        // Send text.done - transitions to Closed (through Completing)
        let done_text = r#"{"type":"text.done"}"#;
        handle_text_message(
            done_text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        // Final state should be Closed
        assert_eq!(session.as_ref().unwrap().state, SessionState::Closed);
    }

    #[tokio::test]
    async fn test_full_session_flow_init_text_done() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry();

        // Initialize session
        let init_text = r#"{"type":"session.init"}"#;
        let should_continue = handle_text_message(
            init_text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;
        assert!(should_continue);
        assert!(session.is_some());
        sender.clear_messages();

        // Send text with two complete sentences.
        // SRX detects "Hello world." as complete because it's followed by " Goodbye world."
        // "Goodbye world." stays in buffer because there's no following text yet.
        let text_msg = r#"{"type":"text","content":"Hello world. Goodbye world."}"#;
        let should_continue = handle_text_message(
            text_msg,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;
        assert!(should_continue);
        // 1 binary frame for the first complete sentence
        assert_eq!(sender.messages().len(), 1);
        assert!(matches!(sender.messages()[0], Message::Binary(_)));

        sender.clear_messages();

        // Send text.done - this flushes "Goodbye world." from buffer
        let done_text = r#"{"type":"text.done"}"#;
        let should_continue = handle_text_message(
            done_text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;
        assert!(should_continue);

        // Verify flushed sentence audio + audio.done was sent
        assert_eq!(sender.messages().len(), 2);
        // First message should be binary audio for the flushed sentence
        assert!(matches!(sender.messages()[0], Message::Binary(_)));
        // Second message should be audio.done
        if let Message::Text(json) = &sender.messages()[1] {
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
                _ => panic!("Expected AudioDone, got {msg:?}"),
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
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry();

        // Initialize session
        let init_text = r#"{"type":"session.init"}"#;
        handle_text_message(
            init_text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;
        sender.clear_messages();

        // Send text with one complete sentence followed by more text (streaming style).
        // SRX needs following text to detect sentence boundaries.
        let text_msg = r#"{"type":"text","content":"Hello world. More text here"}"#;
        handle_text_message(
            text_msg,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        // Should have 1 binary frame for "Hello world."
        assert_eq!(sender.messages().len(), 1);

        // Parse the binary frame
        if let Message::Binary(data) = &sender.messages()[0] {
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
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry_with_mode(MockSynthesisMode::FailInit {
            error: "Test synthesis failure".to_string(),
        });

        // Initialize session
        let init_text = r#"{"type":"session.init"}"#;
        handle_text_message(
            init_text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;
        sender.clear_messages();

        // Send text with one complete sentence followed by more text (streaming style).
        // SRX needs following text to detect sentence boundaries.
        // Synthesis will fail for "Hello world."
        let text_msg = r#"{"type":"text","content":"Hello world. More text"}"#;
        let should_continue = handle_text_message(
            text_msg,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        // Should continue (non-fatal error)
        assert!(should_continue);

        // Should have 1 error message (no binary audio due to failure)
        assert_eq!(sender.messages().len(), 1);

        if let Message::Text(json) = &sender.messages()[0] {
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
                _ => panic!("Expected Error, got {msg:?}"),
            }
        } else {
            panic!("Expected text message");
        }
    }

    #[tokio::test]
    #[allow(clippy::cast_possible_truncation)]
    async fn test_multiple_sentences_sequence_numbers() {
        let mut sender = TestSender::new();
        let mut session = None;
        let registry = create_test_registry();

        // Initialize session
        let init_text = r#"{"type":"session.init"}"#;
        handle_text_message(
            init_text,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;
        sender.clear_messages();

        // Send text with three complete sentences followed by more text.
        // SRX only emits sentences when followed by more text (streaming behavior).
        // "First." and "Second." are followed by more text, so they're emitted.
        // "Third." is followed by " More" so it's also emitted.
        let text_msg = r#"{"type":"text","content":"First. Second. Third. More"}"#;
        handle_text_message(
            text_msg,
            sender.as_bp_sender(),
            &mut session,
            &registry,
            &default_buffer_config(),
        )
        .await;

        // Should have 3 binary frames
        assert_eq!(sender.messages().len(), 3);

        // Verify sequence numbers and sentence indices
        for (i, msg) in sender.messages().iter().enumerate() {
            if let Message::Binary(data) = msg {
                let sequence = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                let sentence_index = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);

                assert_eq!(sequence, i as u32);
                assert_eq!(sentence_index, i as u32);
            } else {
                panic!("Expected binary message at index {i}");
            }
        }

        // Session should track 3 audio chunks
        assert_eq!(session.as_ref().unwrap().audio_sequence, 3);
    }
}
