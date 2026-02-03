//! WebSocket message types for the Yappy TTS protocol
//!
//! This module defines the JSON message types exchanged between client and server
//! over the WebSocket connection, as specified in the WebSocket protocol contract.
//!
//! # Message Flow
//!
//! ```text
//! Client                                    Server
//!    |──── session.init ────────────────────▶|
//!    |◀──────────────────────── session.ready |
//!    |──── text ────────────────────────────▶|
//!    |◀──────────────────── [Binary: audio]   |
//!    |──── text.done ───────────────────────▶|
//!    |◀────────────────────────── audio.done  |
//! ```

use serde::{Deserialize, Serialize};

use crate::audio::AudioFormat;
use crate::session::{CodeBlockMode, SessionId, VoiceConfig};

/// Messages sent from the client to the server.
///
/// All client messages are JSON text frames with a `type` field that identifies
/// the message variant.
///
/// # Example
///
/// ```
/// use yappy_core::message::ClientMessage;
///
/// let init = r#"{"type":"session.init","provider":"kokoro"}"#;
/// let msg: ClientMessage = serde_json::from_str(init).unwrap();
/// assert!(matches!(msg, ClientMessage::SessionInit { .. }));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Initialize a new TTS session. Must be the first message sent.
    ///
    /// # Example JSON
    ///
    /// ```json
    /// {
    ///   "type": "session.init",
    ///   "provider": "kokoro",
    ///   "voice": {"id": "af_bella", "speed": 1.0},
    ///   "audio_format": {"codec": "opus", "sample_rate": 48000, "channels": 1},
    ///   "code_block_mode": "skip"
    /// }
    /// ```
    #[serde(rename = "session.init")]
    SessionInit {
        /// Provider ID from `/providers` endpoint. Uses server default if not specified.
        #[serde(skip_serializing_if = "Option::is_none")]
        provider: Option<String>,

        /// Voice configuration (voice ID, speed, pitch, volume).
        #[serde(skip_serializing_if = "Option::is_none")]
        voice: Option<VoiceConfig>,

        /// Desired audio output format (codec, sample rate, channels).
        #[serde(skip_serializing_if = "Option::is_none")]
        audio_format: Option<AudioFormat>,

        /// How to handle fenced code blocks in input text.
        #[serde(skip_serializing_if = "Option::is_none")]
        code_block_mode: Option<CodeBlockMode>,
    },

    /// Send a text chunk for synthesis. May be sent multiple times.
    ///
    /// # Example JSON
    ///
    /// ```json
    /// {
    ///   "type": "text",
    ///   "content": "Hello, world. This is a test."
    /// }
    /// ```
    #[serde(rename = "text")]
    Text {
        /// UTF-8 text to synthesize (max 64KB).
        content: String,
    },

    /// Signal end of text input.
    ///
    /// After sending this message, the server will flush any buffered text,
    /// send remaining audio, and then send an `audio.done` message.
    ///
    /// # Example JSON
    ///
    /// ```json
    /// {
    ///   "type": "text.done"
    /// }
    /// ```
    #[serde(rename = "text.done")]
    TextDone,
}

/// Messages sent from the server to the client.
///
/// Server messages include both JSON text frames (control messages) and
/// binary frames (audio data). This enum represents only the JSON messages.
/// Audio data is sent as binary frames with a 12-byte header followed by
/// encoded audio bytes.
///
/// # Example
///
/// ```
/// use yappy_core::message::ServerMessage;
///
/// let ready = ServerMessage::SessionReady {
///     session_id: "ses_01HQXYZ123ABC".to_string(),
///     audio_format: yappy_core::AudioFormat::default(),
///     voice: "af_bella".to_string(),
/// };
/// let json = serde_json::to_string(&ready).unwrap();
/// assert!(json.contains(r#""type":"session.ready""#));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Confirms session initialization. Sent in response to `session.init`.
    ///
    /// # Example JSON
    ///
    /// ```json
    /// {
    ///   "type": "session.ready",
    ///   "session_id": "ses_01HQXYZ123ABC",
    ///   "audio_format": {"codec": "opus", "sample_rate": 48000, "channels": 1},
    ///   "voice": "af_bella"
    /// }
    /// ```
    #[serde(rename = "session.ready")]
    SessionReady {
        /// Unique identifier for this session.
        session_id: String,

        /// Negotiated audio output format.
        audio_format: AudioFormat,

        /// Selected voice identifier.
        voice: String,
    },

    /// Signals all audio has been sent. Sent after `text.done` processing completes.
    ///
    /// # Example JSON
    ///
    /// ```json
    /// {
    ///   "type": "audio.done",
    ///   "total_sentences": 5,
    ///   "total_duration_ms": 12340,
    ///   "total_bytes": 98765
    /// }
    /// ```
    #[serde(rename = "audio.done")]
    AudioDone {
        /// Total number of sentences synthesized.
        total_sentences: u32,

        /// Total audio duration in milliseconds.
        total_duration_ms: u64,

        /// Total bytes of audio data sent.
        total_bytes: u64,
    },

    /// Non-fatal error during processing. Connection remains open.
    ///
    /// The `fatal` field is always `false` for this message type.
    /// Use `SessionError` for fatal errors that close the connection.
    ///
    /// # Example JSON
    ///
    /// ```json
    /// {
    ///   "type": "error",
    ///   "code": "synthesis_failed",
    ///   "message": "Failed to synthesize sentence 3",
    ///   "fatal": false,
    ///   "sentence_index": 3
    /// }
    /// ```
    #[serde(rename = "error")]
    Error {
        /// Error code identifying the error type.
        code: String,

        /// Human-readable error description.
        message: String,

        /// Whether this is a fatal error. Always `false` for this variant.
        fatal: bool,

        /// Which sentence failed, if applicable.
        #[serde(skip_serializing_if = "Option::is_none")]
        sentence_index: Option<u32>,
    },

    /// Fatal error. Server will close the connection after sending this message.
    ///
    /// # Example JSON
    ///
    /// ```json
    /// {
    ///   "type": "session.error",
    ///   "code": "provider_unavailable",
    ///   "message": "Provider 'openai' is not configured",
    ///   "alternatives": ["kokoro", "avspeech"]
    /// }
    /// ```
    #[serde(rename = "session.error")]
    SessionError {
        /// Error code identifying the error type.
        code: String,

        /// Human-readable error description.
        message: String,

        /// Available alternatives (e.g., for provider errors).
        #[serde(skip_serializing_if = "Option::is_none")]
        alternatives: Option<Vec<String>>,
    },
}

impl ServerMessage {
    /// Create a `SessionReady` message from session components.
    pub fn session_ready(session_id: &SessionId, audio_format: AudioFormat, voice: &str) -> Self {
        Self::SessionReady {
            session_id: session_id.to_string(),
            audio_format,
            voice: voice.to_string(),
        }
    }

    /// Create an `AudioDone` message with synthesis statistics.
    pub const fn audio_done(
        total_sentences: u32,
        total_duration_ms: u64,
        total_bytes: u64,
    ) -> Self {
        Self::AudioDone {
            total_sentences,
            total_duration_ms,
            total_bytes,
        }
    }

    /// Create a non-fatal `Error` message.
    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Error {
            code: code.into(),
            message: message.into(),
            fatal: false,
            sentence_index: None,
        }
    }

    /// Create a non-fatal `Error` message with sentence index.
    pub fn error_with_sentence(
        code: impl Into<String>,
        message: impl Into<String>,
        sentence_index: u32,
    ) -> Self {
        Self::Error {
            code: code.into(),
            message: message.into(),
            fatal: false,
            sentence_index: Some(sentence_index),
        }
    }

    /// Create a fatal `SessionError` message.
    pub fn session_error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::SessionError {
            code: code.into(),
            message: message.into(),
            alternatives: None,
        }
    }

    /// Create a fatal `SessionError` message with alternatives.
    pub fn session_error_with_alternatives(
        code: impl Into<String>,
        message: impl Into<String>,
        alternatives: Vec<String>,
    ) -> Self {
        Self::SessionError {
            code: code.into(),
            message: message.into(),
            alternatives: Some(alternatives),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::AudioCodec;

    #[test]
    fn test_client_session_init_full() {
        let json = r#"{
            "type": "session.init",
            "provider": "kokoro",
            "voice": {"id": "af_bella", "speed": 1.0, "pitch": 0.0, "volume": 1.0},
            "audio_format": {"codec": "opus", "sample_rate": 48000, "channels": 1},
            "code_block_mode": "skip"
        }"#;

        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::SessionInit {
                provider,
                voice,
                audio_format,
                code_block_mode,
            } => {
                assert_eq!(provider, Some("kokoro".to_string()));
                assert_eq!(voice.as_ref().unwrap().id, "af_bella");
                assert_eq!(audio_format.as_ref().unwrap().codec, AudioCodec::Opus);
                assert_eq!(code_block_mode, Some(CodeBlockMode::Skip));
            }
            _ => panic!("Expected SessionInit"),
        }
    }

    #[test]
    fn test_client_session_init_minimal() {
        let json = r#"{"type": "session.init"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::SessionInit {
                provider,
                voice,
                audio_format,
                code_block_mode,
            } => {
                assert!(provider.is_none());
                assert!(voice.is_none());
                assert!(audio_format.is_none());
                assert!(code_block_mode.is_none());
            }
            _ => panic!("Expected SessionInit"),
        }
    }

    #[test]
    fn test_client_text() {
        let json = r#"{"type": "text", "content": "Hello, world."}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::Text { content } => {
                assert_eq!(content, "Hello, world.");
            }
            _ => panic!("Expected Text"),
        }
    }

    #[test]
    fn test_client_text_done() {
        let json = r#"{"type": "text.done"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        assert!(matches!(msg, ClientMessage::TextDone));
    }

    #[test]
    fn test_server_session_ready() {
        let msg =
            ServerMessage::session_ready(&SessionId::default(), AudioFormat::default(), "af_bella");
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"session.ready""#));
        assert!(json.contains(r#""voice":"af_bella""#));
    }

    #[test]
    fn test_server_session_ready_roundtrip() {
        let json = r#"{
            "type": "session.ready",
            "session_id": "ses_01HQXYZ123ABC",
            "audio_format": {"codec": "opus", "sample_rate": 48000, "channels": 1},
            "voice": "af_bella"
        }"#;

        let msg: ServerMessage = serde_json::from_str(json).unwrap();
        match msg {
            ServerMessage::SessionReady {
                session_id,
                audio_format,
                voice,
            } => {
                assert_eq!(session_id, "ses_01HQXYZ123ABC");
                assert_eq!(audio_format.codec, AudioCodec::Opus);
                assert_eq!(audio_format.sample_rate, 48000);
                assert_eq!(audio_format.channels, 1);
                assert_eq!(voice, "af_bella");
            }
            _ => panic!("Expected SessionReady"),
        }
    }

    #[test]
    fn test_server_audio_done() {
        let msg = ServerMessage::audio_done(5, 12340, 98765);
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"audio.done""#));
        assert!(json.contains(r#""total_sentences":5"#));
        assert!(json.contains(r#""total_duration_ms":12340"#));
        assert!(json.contains(r#""total_bytes":98765"#));
    }

    #[test]
    fn test_server_error() {
        let msg = ServerMessage::error_with_sentence(
            "synthesis_failed",
            "Failed to synthesize sentence 3",
            3,
        );
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"error""#));
        assert!(json.contains(r#""code":"synthesis_failed""#));
        assert!(json.contains(r#""fatal":false"#));
        assert!(json.contains(r#""sentence_index":3"#));
    }

    #[test]
    fn test_server_session_error() {
        let msg = ServerMessage::session_error_with_alternatives(
            "provider_unavailable",
            "Provider 'openai' is not configured",
            vec!["kokoro".to_string(), "avspeech".to_string()],
        );
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"session.error""#));
        assert!(json.contains(r#""code":"provider_unavailable""#));
        assert!(json.contains(r#""alternatives":["kokoro","avspeech"]"#));
    }

    #[test]
    fn test_server_error_no_sentence_index_omitted() {
        let msg = ServerMessage::error("rate_limited", "Rate limit exceeded");
        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains("sentence_index"));
    }

    #[test]
    fn test_server_session_error_no_alternatives_omitted() {
        let msg = ServerMessage::session_error("internal_error", "Unexpected error");
        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains("alternatives"));
    }

    #[test]
    fn test_client_message_serialize_roundtrip() {
        let msg = ClientMessage::SessionInit {
            provider: Some("kokoro".to_string()),
            voice: None,
            audio_format: None,
            code_block_mode: Some(CodeBlockMode::ReadLiterally),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();

        match parsed {
            ClientMessage::SessionInit {
                provider,
                code_block_mode,
                ..
            } => {
                assert_eq!(provider, Some("kokoro".to_string()));
                assert_eq!(code_block_mode, Some(CodeBlockMode::ReadLiterally));
            }
            _ => panic!("Expected SessionInit"),
        }
    }
}
