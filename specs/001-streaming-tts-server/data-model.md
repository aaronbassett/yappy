# Data Model: Yappy Streaming TTS Server

**Date**: 2026-02-03
**Source**: [spec.md](./spec.md) Key Entities section

---

## Core Entities

### Session

Represents a WebSocket connection with its chosen provider, voice, audio format, and sentence buffer state.

```rust
/// Unique session identifier (TypeID format)
pub struct SessionId(TypeId);

/// Session state for an active WebSocket connection
pub struct Session {
    /// Unique identifier for this session
    pub id: SessionId,

    /// Selected TTS provider for this session
    pub provider_id: ProviderId,

    /// Selected voice configuration
    pub voice: VoiceConfig,

    /// Negotiated audio output format
    pub audio_format: AudioFormat,

    /// Sentence buffer for text accumulation
    pub buffer: SentenceBuffer,

    /// Session creation timestamp
    pub created_at: Instant,

    /// Last activity timestamp (for idle timeout)
    pub last_activity: Instant,

    /// Current session state
    pub state: SessionState,

    /// Code block handling mode
    pub code_block_mode: CodeBlockMode,
}

/// Session lifecycle states
pub enum SessionState {
    /// Session initialized, waiting for text
    Ready,
    /// Actively processing text and streaming audio
    Streaming,
    /// Client sent text.done, flushing remaining audio
    Completing,
    /// Session ended (normal or error)
    Closed,
}

/// How to handle fenced code blocks in input text
pub enum CodeBlockMode {
    /// Skip code blocks entirely (default)
    Skip,
    /// Read code blocks literally
    ReadLiterally,
    /// Announce "code block" then skip content
    AnnounceAndSkip,
}
```

### Provider

A TTS backend implementation that converts text to audio streams.

```rust
/// Unique provider identifier
pub struct ProviderId(String);

/// TTS Provider trait - implemented by each backend
#[async_trait]
pub trait TtsProvider: Send + Sync {
    /// Provider metadata
    fn metadata(&self) -> ProviderMetadata;

    /// Check if provider is ready to accept requests
    async fn health_check(&self) -> ProviderStatus;

    /// Synthesize text to audio stream
    /// Returns a stream of audio chunks
    async fn synthesize(
        &self,
        text: &str,
        voice: &VoiceConfig,
        format: AudioFormat,
        cancel: CancellationToken,
    ) -> Result<AudioStream, ProviderError>;
}

/// Provider metadata exposed via /providers endpoint
pub struct ProviderMetadata {
    /// Unique identifier (e.g., "kokoro", "openai", "avspeech")
    pub id: ProviderId,

    /// Human-readable name
    pub name: String,

    /// Description of the provider
    pub description: String,

    /// Available voices
    pub voices: Vec<VoiceInfo>,

    /// Supported audio output formats
    pub supported_formats: Vec<AudioFormat>,

    /// Provider-specific options schema (JSON Schema)
    pub options_schema: Option<serde_json::Value>,
}

/// Provider readiness status
pub enum ProviderStatus {
    /// Ready to accept synthesis requests
    Available,

    /// Provider is compiled in but not configured
    NotConfigured { reason: String },

    /// Provider is temporarily unavailable
    Unavailable { reason: String },
}
```

### Voice

A specific voice configuration within a provider.

```rust
/// Voice information for discovery
pub struct VoiceInfo {
    /// Voice identifier (provider-specific)
    pub id: String,

    /// Human-readable name
    pub name: String,

    /// Language code (BCP 47, e.g., "en-US")
    pub language: String,

    /// Voice gender if applicable
    pub gender: Option<VoiceGender>,

    /// Sample audio URL if available
    pub sample_url: Option<String>,
}

/// Voice configuration for synthesis
pub struct VoiceConfig {
    /// Voice identifier
    pub id: String,

    /// Speech rate multiplier (0.5 - 2.0, default 1.0)
    pub speed: f32,

    /// Pitch adjustment if supported (-1.0 to 1.0, default 0.0)
    pub pitch: f32,

    /// Volume adjustment (0.0 to 1.0, default 1.0)
    pub volume: f32,
}

pub enum VoiceGender {
    Male,
    Female,
    Neutral,
}
```

### SentenceBuffer

Accumulates text chunks and emits complete sentences for synthesis.

```rust
/// Sentence buffer configuration
pub struct BufferConfig {
    /// Flush timeout when no sentence boundary detected (default: 500ms)
    pub flush_timeout: Duration,

    /// Maximum buffer size before forced flush (default: 4KB)
    pub max_size: usize,
}

/// Sentence buffer state
pub struct SentenceBuffer {
    /// Accumulated text not yet emitted
    buffer: String,

    /// Configuration
    config: BufferConfig,

    /// SRX segmenter for sentence detection
    segmenter: SrxSegmenter,

    /// Current sentence index (for error correlation)
    sentence_index: u32,

    /// Whether currently inside a code block
    in_code_block: bool,
}

impl SentenceBuffer {
    /// Add text chunk to buffer, return complete sentences
    pub fn push(&mut self, text: &str) -> Vec<Sentence>;

    /// Flush remaining buffer content (called on text.done)
    pub fn flush(&mut self) -> Option<Sentence>;

    /// Check if flush timeout has elapsed
    pub fn should_timeout_flush(&self) -> bool;
}

/// A complete sentence ready for synthesis
pub struct Sentence {
    /// The sentence text
    pub text: String,

    /// Sentence index for correlation
    pub index: u32,
}
```

### AudioChunk

A fragment of encoded audio data with metadata.

```rust
/// Audio output format specification
pub struct AudioFormat {
    /// Codec (opus, pcm, mp3)
    pub codec: AudioCodec,

    /// Sample rate in Hz (e.g., 24000, 48000)
    pub sample_rate: u32,

    /// Number of channels (1 = mono, 2 = stereo)
    pub channels: u8,

    /// Bits per sample (for PCM)
    pub bits_per_sample: Option<u8>,
}

pub enum AudioCodec {
    Opus,
    Pcm,
    Mp3,
}

/// A chunk of encoded audio
pub struct AudioChunk {
    /// Sequence number within session
    pub sequence: u32,

    /// Which sentence this audio belongs to
    pub sentence_index: u32,

    /// Encoded audio data
    pub data: Bytes,

    /// Duration of this chunk in milliseconds
    pub duration_ms: u32,
}

/// Stream of audio chunks from provider
pub type AudioStream = Pin<Box<dyn Stream<Item = Result<AudioChunk, ProviderError>> + Send>>;
```

### Configuration

Server, provider, and buffer settings loaded from TOML file.

```rust
/// Root configuration structure
#[derive(Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub providers: ProvidersConfig,
    pub buffer: BufferConfig,
}

/// Server configuration
#[derive(Deserialize)]
pub struct ServerConfig {
    /// Bind address (default: "127.0.0.1")
    pub host: String,

    /// Port (default: 3000)
    pub port: u16,

    /// Log level (default: "info")
    pub log_level: String,

    /// Idle connection timeout (default: 5 minutes)
    pub idle_timeout_secs: u64,

    /// Synthesis request timeout (default: 30 seconds)
    pub synthesis_timeout_secs: u64,
}

/// Provider configuration section
#[derive(Deserialize)]
pub struct ProvidersConfig {
    /// Default provider ID
    pub default: String,

    /// OpenAI provider settings
    pub openai: Option<OpenAiConfig>,

    /// Kokoro provider settings
    pub kokoro: Option<KokoroConfig>,

    /// AVSpeech provider settings (macOS only)
    pub avspeech: Option<AvSpeechConfig>,
}

#[derive(Deserialize)]
pub struct OpenAiConfig {
    /// API key (supports $ENV_VAR syntax)
    pub api_key: String,

    /// Model to use (default: "tts-1")
    pub model: Option<String>,
}

#[derive(Deserialize)]
pub struct KokoroConfig {
    /// Path to ONNX model file (or "auto" for HF download)
    pub model_path: String,

    /// Path to voices file
    pub voices_path: String,
}

#[derive(Deserialize)]
pub struct AvSpeechConfig {
    /// Whether to enable (default: true on macOS)
    pub enabled: bool,
}
```

---

## WebSocket Message Types

### Client → Server Messages

```rust
/// Messages sent by client
#[derive(Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    /// Initialize session with configuration
    #[serde(rename = "session.init")]
    SessionInit {
        /// Provider ID (optional, uses default if omitted)
        provider: Option<String>,

        /// Voice configuration
        voice: Option<VoiceConfig>,

        /// Requested audio format
        audio_format: Option<AudioFormatRequest>,

        /// Code block handling mode
        code_block_mode: Option<CodeBlockMode>,
    },

    /// Text chunk to synthesize
    #[serde(rename = "text")]
    Text {
        /// Text content
        content: String,
    },

    /// Signal end of text input
    #[serde(rename = "text.done")]
    TextDone,
}
```

### Server → Client Messages

```rust
/// Messages sent by server
#[derive(Serialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    /// Session ready confirmation
    #[serde(rename = "session.ready")]
    SessionReady {
        session_id: String,
        audio_format: AudioFormat,
        voice: String,
    },

    /// Audio chunk (binary frame, not JSON)
    /// Sent as raw binary WebSocket frame with header:
    /// [sequence:u32][sentence_index:u32][duration_ms:u32][data...]
    #[serde(skip)]
    AudioChunk(AudioChunk),

    /// All audio complete
    #[serde(rename = "audio.done")]
    AudioDone {
        /// Total sentences processed
        total_sentences: u32,

        /// Total audio duration in milliseconds
        total_duration_ms: u64,

        /// Total bytes sent
        total_bytes: u64,
    },

    /// Non-fatal error
    #[serde(rename = "error")]
    Error {
        code: String,
        message: String,
        fatal: bool,
        sentence_index: Option<u32>,
    },

    /// Session error (fatal)
    #[serde(rename = "session.error")]
    SessionError {
        code: String,
        message: String,
        alternatives: Option<Vec<String>>,
    },
}
```

---

## Error Types

```rust
/// Provider-specific errors (non-fatal by default)
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("Synthesis failed: {message}")]
    SynthesisFailed { message: String },

    #[error("Rate limited, retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u32 },

    #[error("Provider timeout after {timeout_secs}s")]
    Timeout { timeout_secs: u32 },

    #[error("Invalid voice: {voice_id}")]
    InvalidVoice { voice_id: String, available: Vec<String> },

    #[error("Unsupported format: {format:?}")]
    UnsupportedFormat { format: AudioFormat },

    #[error("Provider not configured: {reason}")]
    NotConfigured { reason: String },
}

/// Session-level errors (fatal)
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("Protocol violation: {message}")]
    ProtocolViolation { message: String },

    #[error("Provider unavailable: {provider_id}")]
    ProviderUnavailable { provider_id: String, alternatives: Vec<String> },

    #[error("Configuration error: {message}")]
    ConfigError { message: String },

    #[error("Internal server error")]
    Internal,
}
```

---

## State Transitions

```
┌──────────────────────────────────────────────────────────────┐
│                      Session Lifecycle                        │
├──────────────────────────────────────────────────────────────┤
│                                                               │
│  [WebSocket Connect]                                          │
│         │                                                     │
│         ▼                                                     │
│  ┌─────────────┐  session.init   ┌─────────────┐             │
│  │  Connected  │ ───────────────▶│    Ready    │             │
│  └─────────────┘                 └─────────────┘             │
│         │                               │                     │
│         │ (timeout)                     │ text                │
│         ▼                               ▼                     │
│  ┌─────────────┐                 ┌─────────────┐             │
│  │   Closed    │◀────────────────│  Streaming  │◀──┐         │
│  └─────────────┘  fatal error    └─────────────┘   │         │
│         ▲                               │          │ text    │
│         │                               │ text.done│         │
│         │                               ▼          │         │
│         │                        ┌─────────────┐   │         │
│         └────────────────────────│ Completing  │───┘         │
│                  audio.done      └─────────────┘             │
│                                                               │
└──────────────────────────────────────────────────────────────┘
```

---

## Validation Rules

### Session Init
- `provider`: Must be available (status != NotConfigured/Unavailable)
- `voice.id`: Must exist in provider's voice list
- `voice.speed`: Must be 0.5 - 2.0
- `voice.pitch`: Must be -1.0 - 1.0
- `voice.volume`: Must be 0.0 - 1.0
- `audio_format.codec`: Must be supported by provider or transcodable

### Text Input
- Must be valid UTF-8
- Empty/whitespace-only text is ignored (no error)
- Maximum single message size: 64KB

### Configuration
- `server.host`: Valid IP address or hostname
- `server.port`: Valid port number (1-65535)
- `providers.default`: Must reference a compiled provider
- API keys: Must not be logged or exposed in errors
