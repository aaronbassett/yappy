# Feature Specification: Yappy Streaming TTS Server

**Feature Branch**: `001-streaming-tts-server`
**Created**: 2026-02-03
**Status**: Draft
**Codebase Documentation**: See [.sdd/codebase/](.sdd/codebase/) for technical details

## Overview

Yappy is a standalone server that accepts streaming text input and produces near-realtime audio output via text-to-speech. It is a stateless streaming system — text goes in, audio comes out — with a pluggable provider system that allows different TTS backends to be selected per session.

The server is client-agnostic. Any application that can open a WebSocket connection can use Yappy. There is no bundled UI, no client, and no opinion about what lives on the other end of the wire.

## Clarifications

### Session 2026-02-03

- Q: What WebSocket frame format should be used for audio data? → A: Binary-only. Clients must handle binary WebSocket frames; no JSON fallback for audio data.
- Q: What determines if an error is fatal (session-terminating) vs non-fatal? → A: Provider-specific errors (synthesis failures, rate limits, timeouts) are non-fatal. Connection/protocol errors (malformed messages, WebSocket errors, server failures) are fatal.
- Q: What are the default values for sentence buffer flush timeout and max buffer size? → A: 500ms flush timeout, 4KB max buffer size.
- Q: What happens if default provider is unavailable and client doesn't specify one? → A: Fail with session.error listing available provider alternatives. No silent fallback to avoid unexpected voice/cost changes.
- Q: How should code blocks (fenced with backticks) be handled by default? → A: Skip code blocks by default. Configurable to read literally or announce-and-skip.

## User Scenarios & Testing

### User Story 1 - Basic Text-to-Speech Session (Priority: P1)

A developer connects to the Yappy server via WebSocket to convert streaming text (e.g., from an LLM) into audio that can be played back in real-time.

**Why this priority**: This is the core value proposition — streaming text in, streaming audio out. Without this, there is no product.

**Independent Test**: Connect via WebSocket, send text chunks, receive audio chunks, verify audio is playable and matches expected duration/quality.

**Acceptance Scenarios**:

1. **Given** a running server with at least one configured provider, **When** a client connects and sends a session init message with a valid provider, **Then** the server responds with session.ready containing the session ID and negotiated audio format.

2. **Given** an established session, **When** the client sends text chunks with the text "Hello, world.", **Then** the server emits audio chunks that, when concatenated, produce audible speech of the input text.

3. **Given** an established session receiving text, **When** the client sends text.done, **Then** the server flushes any buffered text, synthesizes remaining audio, and sends audio.done with summary statistics.

4. **Given** a session with streaming text, **When** new text chunks arrive faster than complete sentences form, **Then** the sentence buffer accumulates text and only emits audio once complete sentences are detected.

---

### User Story 2 - Provider Discovery and Selection (Priority: P1)

A developer queries the server to discover available TTS providers and their capabilities, then selects the most appropriate provider for their use case.

**Why this priority**: Multi-provider support is a fundamental differentiator. Users need to know what's available and choose appropriately.

**Independent Test**: Call the /providers endpoint, verify the response lists all compiled providers with their status, voices, and capabilities. Then create a session specifying a particular provider.

**Acceptance Scenarios**:

1. **Given** a server built with multiple provider feature flags, **When** a client calls GET /providers, **Then** the response includes all compiled providers with their id, name, description, status, voices, supported audio formats, and options schema.

2. **Given** a provider that is compiled in but not configured (e.g., OpenAI without API key), **When** a client calls GET /providers, **Then** that provider appears with status "not_configured" and a human-readable reason.

3. **Given** available providers A and B, **When** a client creates a session requesting provider B, **Then** the session uses provider B and audio is synthesized using that provider's engine.

4. **Given** a server with the default provider unavailable (e.g., model file missing), **When** a client connects without specifying a provider, **Then** the server responds with session.error containing code "provider_unavailable", a message explaining the default provider is unavailable, and an "alternatives" array listing other available providers.

---

### User Story 3 - Voice Selection and Configuration (Priority: P2)

A developer selects a specific voice and configures synthesis options (speed, pitch) when creating a session to customize the audio output.

**Why this priority**: Voice customization is expected functionality, but the system works with defaults if not specified.

**Independent Test**: Create a session with a specific voice and custom speed setting, verify the audio output reflects those settings.

**Acceptance Scenarios**:

1. **Given** a provider with multiple voices, **When** a client creates a session specifying voice "af_bella", **Then** the session.ready message confirms the selected voice and subsequent audio uses that voice.

2. **Given** a provider that supports speed adjustment, **When** a client creates a session with options.speed = 1.5, **Then** the synthesized audio plays approximately 50% faster than normal.

3. **Given** a session init with an unsupported voice for the selected provider, **When** the server processes the request, **Then** it responds with session.error containing code "invalid_voice" and suggests available alternatives.

---

### User Story 4 - Health Monitoring (Priority: P2)

An operations engineer monitors the server's health and provider availability to ensure the service is operational.

**Why this priority**: Health checks are essential for production deployments but not required for basic functionality.

**Independent Test**: Call GET /health, verify response includes server status, uptime, and per-provider availability status.

**Acceptance Scenarios**:

1. **Given** a running server, **When** a client calls GET /health, **Then** the response includes status "ok", uptime in seconds, and a providers object listing each provider's readiness.

2. **Given** a server where one provider has become unavailable (e.g., model file deleted), **When** GET /health is called, **Then** the affected provider shows "unavailable" with a diagnostic message, but overall status remains "ok" if other providers work.

---

### User Story 5 - Audio Format Negotiation (Priority: P2)

A developer requests a specific audio format (Opus, PCM, MP3) during session setup to match their client's playback capabilities.

**Why this priority**: Format flexibility enables broader client support, but Opus default covers most cases.

**Independent Test**: Create sessions requesting different audio formats, verify the audio.chunk data is encoded in the requested format.

**Acceptance Scenarios**:

1. **Given** a provider that supports multiple output formats, **When** a client requests audio_format "opus" during session init, **Then** session.ready confirms opus codec and audio chunks are Opus-encoded.

2. **Given** a provider that outputs PCM natively, **When** a client requests opus format, **Then** the server transcodes to Opus and delivers audio in the requested format.

3. **Given** a session init requesting an unsupported format for the selected provider, **When** transcoding is not available, **Then** the server responds with session.error explaining the format mismatch.

---

### User Story 6 - Error Recovery During Streaming (Priority: P2)

A developer's application handles non-fatal synthesis errors gracefully, continuing to receive audio for successful sentences while being notified of failures.

**Why this priority**: Robust error handling improves user experience but isn't required for happy-path functionality.

**Independent Test**: Send text that triggers a provider error mid-stream, verify the client receives an error message for that sentence but continues receiving audio for subsequent sentences.

**Acceptance Scenarios**:

1. **Given** a session streaming multiple sentences, **When** synthesis fails for sentence 3, **Then** the client receives an error message with fatal: false, sentence_index: 3, and continues receiving audio for sentences 4+.

2. **Given** a fatal error (e.g., cloud provider connection lost), **When** the error occurs, **Then** the client receives an error with fatal: true, and the server closes the WebSocket connection cleanly.

---

### User Story 7 - Sentence Buffer Intelligence (Priority: P3)

A developer streams text from an LLM that arrives in fragments, and the system intelligently accumulates and segments text into natural sentences before synthesis.

**Why this priority**: Smart buffering improves audio quality and reduces choppy output, but basic accumulation works without it.

**Independent Test**: Stream text with abbreviations (Dr., Mrs.), decimal numbers (3.14), and ellipses — verify these don't trigger premature sentence breaks.

**Acceptance Scenarios**:

1. **Given** incoming text "Dr. Smith arrived at 3.14 p.m.", **When** the buffer processes this text, **Then** it emits the entire text as one sentence, not splitting on "Dr." or "3.".

2. **Given** a pause in incoming text longer than the configured flush timeout, **When** the buffer has accumulated partial text, **Then** it flushes the buffered text even without a sentence terminator.

3. **Given** incoming text exceeding the maximum buffer size without punctuation, **When** the buffer reaches capacity, **Then** it splits at a clause boundary (comma) or word boundary rather than mid-word.

---

### User Story 8 - Concurrent Session Support (Priority: P3)

Multiple clients connect simultaneously and stream text independently without interference or degraded performance.

**Why this priority**: Concurrency support is important for production use but not required for initial single-user scenarios.

**Independent Test**: Open 10 simultaneous WebSocket connections, stream different text through each, verify all sessions complete successfully with correct audio.

**Acceptance Scenarios**:

1. **Given** 10 concurrent client connections, **When** each streams different text simultaneously, **Then** each session receives its own audio output without cross-contamination.

2. **Given** concurrent sessions using different providers, **When** sessions operate in parallel, **Then** each session uses its assigned provider correctly.

---

### User Story 9 - Graceful Shutdown (Priority: P3)

When the server receives a shutdown signal, it completes in-progress audio synthesis and closes connections cleanly.

**Why this priority**: Graceful shutdown prevents data loss but is not required for development/testing.

**Independent Test**: Start a streaming session, send SIGTERM to server, verify the session receives any pending audio and the connection closes cleanly.

**Acceptance Scenarios**:

1. **Given** active streaming sessions, **When** the server receives SIGTERM, **Then** it stops accepting new connections, completes in-progress audio chunks, sends audio.done to active sessions, and exits cleanly.

---

### Edge Cases

- What happens when a client disconnects mid-stream? Server should cancel synthesis for that session, releasing resources.
- What happens when a provider returns empty audio? Server should emit an error for that sentence and continue.
- What happens when text contains only whitespace? Server should ignore empty input, not error.
- What happens when client sends text WebSocket frames instead of binary for audio-related messages? Server should reject with a clear protocol error (audio uses binary frames only).
- What happens when text contains code blocks (fenced with backticks)? Default behavior is to skip code blocks. Configurable to: read literally, or announce "code block" then skip.

## Requirements

### Functional Requirements

#### Server & API

- **FR-001**: System MUST expose a WebSocket endpoint for bidirectional streaming communication.
- **FR-002**: System MUST expose an HTTP GET /health endpoint returning server status and provider availability.
- **FR-003**: System MUST expose an HTTP GET /providers endpoint returning full provider metadata including voices and capabilities.
- **FR-004**: System MUST accept session configuration (provider, voice, audio format, options) at connection time only.
- **FR-005**: System MUST generate unique session IDs for each WebSocket connection.

#### Session Management

- **FR-006**: System MUST maintain per-session state including chosen provider, voice config, and sentence buffer.
- **FR-007**: System MUST handle session lifecycle: creation, streaming, completion/disconnection.
- **FR-008**: System MUST clean up session resources when a client disconnects.
- **FR-009**: System MUST handle backpressure when audio generation outpaces client consumption. When the outbound queue exceeds 32 audio frames, the server shall pause provider generation. When the queue drains below 16 frames, generation resumes. Buffered text is held in SentenceBuffer; no text loss occurs.

#### Text Processing

- **FR-010**: System MUST accumulate incoming text chunks and emit complete sentences to the TTS provider.
- **FR-011**: Sentence buffer MUST handle common abbreviations without false sentence breaks (Dr., Mrs., U.S.A., etc.).
- **FR-012**: Sentence buffer MUST handle decimal numbers without false sentence breaks (3.14, 2.5, etc.).
- **FR-013**: Sentence buffer MUST flush on configurable timeout when no sentence boundary is detected.
- **FR-014**: Sentence buffer MUST split overlong text at clause or word boundaries when maximum buffer size is exceeded.
- **FR-015**: System MUST flush remaining buffered text when text.done is received.
- **FR-016a**: Sentence buffer MUST detect and handle fenced code blocks (triple backticks). Default behavior: skip code blocks. Configurable options: read literally, or announce-and-skip. Code blocks must be fully closed to be recognized; incomplete or unclosed fences are treated as literal text. Nested code blocks are not supported; the first closing fence ends the block. "Announce" mode speaks exactly the text "code block".

#### Audio Output

- **FR-016**: System MUST stream audio as chunks, not waiting for complete synthesis before sending. Audio chunks MUST be sent as binary WebSocket frames (not JSON-encoded).
- **FR-017**: System MUST support Opus audio format as the default output.
- **FR-018**: System MUST support PCM (raw) audio format for clients requiring uncompressed samples.
- **FR-019**: System SHOULD support MP3 audio format for maximum compatibility.
- **FR-020**: System MUST communicate the negotiated audio format (codec, sample rate, channels) during session setup.

#### Provider System

- **FR-021**: System MUST support multiple TTS providers compiled via feature flags.
- **FR-022**: System MUST discover and register available providers at startup based on compiled features.
- **FR-023**: System MUST validate provider configuration and report readiness status for each provider.
- **FR-024**: System MUST allow clients to select provider per-session.
- **FR-024a**: System SHOULD support voice pitch adjustment (-1.0 to 1.0 range) if the selected provider supports it. If a provider does not support pitch adjustment, the pitch parameter is silently ignored.
- **FR-025**: System MUST fall back to configured default provider when client doesn't specify one. If the default provider is unavailable, the system MUST fail with session.error listing available alternatives (no silent fallback).
- **FR-026**: Providers MUST expose metadata: name, description, voices, capabilities, supported audio formats.
- **FR-027**: Providers MUST support cancellation when a client disconnects mid-synthesis. Cancellation shall be cooperative — providers check for cancellation signals and stop work within 5 seconds maximum. If a provider does not respond to cancellation within 5 seconds, the server forcefully terminates the synthesis task and logs a warning.

#### Built-in Providers

- **FR-028**: System MUST include AVSpeechSynthesizer provider for macOS (feature-gated, macOS only).
- **FR-029**: System MUST include Kokoro 82M ONNX provider (feature-gated, cross-platform).
- **FR-030**: System MUST include OpenAI TTS provider (feature-gated, cross-platform).
- **FR-031**: AVSpeechSynthesizer provider MUST stream audio buffers as they're generated, not batch.
- **FR-032**: Kokoro provider MUST check for required model files at startup and report status clearly.
- **FR-033**: OpenAI provider MUST handle rate limiting with appropriate back-off or error reporting.

#### Configuration

- **FR-034**: System MUST load configuration from a TOML file.
- **FR-035**: Configuration MUST support environment variable references for secrets (e.g., "$OPENAI_API_KEY").
- **FR-036**: Configuration MUST include server settings: bind address, port, log level.
- **FR-037**: Configuration MUST include default provider selection.
- **FR-038**: Configuration MUST include provider-specific settings sections.
- **FR-039**: Configuration MUST include sentence buffer settings: flush timeout (default: 500ms), max buffer size (default: 4KB).

#### Error Handling

- **FR-040**: System MUST report non-fatal errors inline without closing the connection. Non-fatal errors include: provider synthesis failures, rate limiting, provider timeouts, and invalid voice/format requests.
- **FR-041**: System MUST report fatal errors and close the WebSocket connection cleanly. Fatal errors include: protocol violations (malformed messages), WebSocket connection errors, and unrecoverable server failures.
- **FR-042**: Error messages MUST include a code and human-readable message.
- **FR-043**: Errors MUST include sentence_index when applicable for client correlation.
- **FR-044**: Provider failures MUST NOT crash the server; they should produce session-scoped errors.
- **FR-045**: System MUST enforce a 10-second timeout for session.init message after WebSocket connection. Connections exceeding this timeout shall be closed with session.error code "protocol_violation" and message "Session init timeout".

### Key Entities

- **Session**: Represents a WebSocket connection with its chosen provider, voice, audio format, sentence buffer state, and unique session ID.
- **Provider**: A TTS backend implementation that converts text to audio streams. Has metadata (name, voices, capabilities) and synthesis capability.
- **Voice**: A specific voice configuration within a provider, with attributes like language, gender, and name.
- **SentenceBuffer**: Accumulates text chunks and emits complete sentences for synthesis. Handles boundary detection and overflow.
- **AudioChunk**: A fragment of encoded audio data with sequence number and sentence association.
- **Configuration**: Server, provider, and buffer settings loaded from TOML file.

## Success Criteria

### Measurable Outcomes

- **SC-001**: First audio byte reaches the client within 500ms of a complete sentence being available (local providers) or 1000ms (cloud providers).
- **SC-002**: Server supports at least 10 concurrent streaming sessions without exceeding 20% latency increase or 5% error rate increase compared to single-session baseline on standard hardware (4-core CPU, 8GB RAM).
- **SC-003**: 99% of sessions complete without fatal errors under normal operating conditions (defined as: valid protocol-compliant requests, configured provider limits not exceeded, no external network failures for cloud providers).
- **SC-004**: Provider discovery endpoint responds in under 50ms.
- **SC-005**: Health check endpoint responds in under 10ms.
- **SC-006**: Session setup (init to ready) completes in under 100ms for local providers, under 500ms for cloud providers requiring validation.
- **SC-007**: Server graceful shutdown completes within 5 seconds, delivering pending audio to active sessions.
- **SC-008**: Sentence buffer correctly handles 95%+ of the standard abbreviation test set without false splits. Test set includes: Dr., Mrs., Mr., Ms., Prof., U.S.A., U.K., Inc., Ltd., Corp., e.g., i.e., etc., vs., a.m., p.m., Jr., Sr., Ph.D., M.D.
- **SC-009**: Audio output matches requested format (Opus/PCM/MP3) in 100% of successful sessions.
- **SC-010**: Zero sensitive data (API keys, input text) appears in server logs.

## Assumptions

- Clients can establish WebSocket connections and handle binary WebSocket frames for audio data.
- For local providers (AVSpeech, Kokoro), required system dependencies and model files are installed by the user.
- For cloud providers (OpenAI), users provide valid API credentials.
- Network latency for cloud providers is reasonable (under 500ms round-trip) for realtime use cases.
- Server runs on macOS or Linux; Windows is not actively targeted but not architecturally precluded.
- Unicode text input is supported; text encoding is UTF-8.
- Audio transcoding (PCM to Opus) adds acceptable CPU overhead for the use case.

## Out of Scope

The following are explicitly not part of this feature:

- **Desktop application**: No GUI, tray icon, or windowed interface.
- **Transcript storage**: No database, search, or persistence of synthesized text.
- **Dynamic plugin loading**: No WASM, dylib, or runtime provider loading. Providers are compiled in.
- **HTTP Bridge Provider**: Runtime extensibility via HTTP proxying to external TTS services.
- **Voice cloning**: Uploading reference audio to create custom voices.
- **SSML support**: Accepting Speech Synthesis Markup Language input.
- **Client SDK/library**: Packaged client libraries for Rust, Swift, Python.
- **Authentication**: Built-in auth mechanisms; assumes trusted network or reverse proxy handles auth.
- **Hot configuration reload**: Config changes require server restart.
- **Model download CLI**: Built-in commands to download provider model files.

## Non-Functional Requirements

### Security

- Server MUST bind to localhost (127.0.0.1) by default. Binding to 0.0.0.0 requires explicit configuration.
- System MUST warn if configuration file has overly permissive permissions (world-readable).
- API keys MUST NOT appear in log output under any circumstances.
- Input text MUST NOT be logged or cached beyond what's necessary for immediate synthesis.

### Observability

- System MUST use structured logging with session IDs included in all session-related log entries.
- Log levels MUST be configurable (error, warn, info, debug, trace).
- System SHOULD distinguish between informational logs (stderr) and data output.
- All requests SHOULD be traceable with unique request IDs propagated through the processing pipeline.
- Errors SHOULD be logged with full context including session ID, provider, and relevant state.

### Reliability

- No panics in production code paths. All errors handled and communicated.
- Provider failures contained to affected session; server continues operating.
- Graceful shutdown completes in-progress work before exiting.
- System MUST enforce timeouts: synthesis requests timeout after 30 seconds, idle WebSocket connections close after 5 minutes.
- System MUST limit concurrent synthesis operations per provider to prevent resource exhaustion.

### Platform Support

- Primary target: macOS (Apple Silicon / aarch64-apple-darwin)
- Secondary target: Linux (x86_64-unknown-linux-gnu)
- Not targeted: Windows (not precluded but not tested)

### Development Standards

- Project MUST include pre-commit hooks (using lefthook) to enforce code formatting and linting before commits.
- Pre-commit hooks MUST run `cargo fmt --check` and `cargo clippy` on staged files.
- Project MUST include a Justfile with standardized commands for common development tasks (build, test, run, lint, format).
- Project MUST include GitHub Actions workflows for:
  - Running tests on pull requests and pushes to main
  - Running linting (clippy) and format checks
  - Building release binaries for supported platforms (macOS, Linux)
- All CI checks MUST pass before merging pull requests.

## Open Questions for Implementation

These decisions were resolved during implementation planning (see research.md):

1. **Audio transcoding responsibility**: ✅ RESOLVED - Server handles internal transcoding for supported format combinations (PCM→Opus, PCM→MP3). Uses `audiopus` for Opus encoding and `mp3lame-encoder` for MP3. Unsupported combinations (e.g., requesting a format neither native to provider nor transcodable) return session.error with code "invalid_format".

2. **Sentence buffer implementation**: ✅ RESOLVED - Use `srx` crate with custom SRX rules as primary implementation. If srx crate unavailable, fall back to `unicode-segmentation` with custom abbreviation handling. Custom rules handle abbreviations (Dr., Mrs., etc.) and decimal numbers.

3. **Model file management for Kokoro**: ✅ RESOLVED - Use `hf-hub` crate for lazy download on first use. Model files are cached in platform-appropriate location (~/.cache/yappy on Linux, ~/Library/Caches/yappy on macOS). CLI pre-download is deferred to future enhancement (out of scope for v1).
