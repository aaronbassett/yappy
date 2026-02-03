# Yappy - Streaming TTS Server

A standalone server that accepts streaming text input via WebSocket and produces near-realtime audio output through pluggable TTS providers.

## Active Technologies

- **Language**: Rust 2021 edition
- **Web Framework**: Axum (HTTP + WebSocket)
- **Async Runtime**: Tokio
- **Serialization**: Serde + serde_json, TOML for config
- **Logging**: tracing + tracing-subscriber
- **Audio Encoding**: audiopus (Opus), mp3lame-encoder (MP3)
- **Text Processing**: srx (sentence segmentation), punkt (training-based)
- **ONNX Inference**: ort (ONNX Runtime), tract (pure Rust fallback)
- **Testing**: cargo test

## Project Structure

```
crates/
├── yappy-core/           # Shared types: TtsProvider trait, Session, AudioChunk
├── yappy-server/         # Axum server binary, CLI
├── yappy-provider-kokoro/    # Kokoro ONNX provider (feature: kokoro)
├── yappy-provider-openai/    # OpenAI TTS API (feature: openai-tts)
└── yappy-provider-avspeech/  # macOS AVSpeech (feature: avspeech)

specs/001-streaming-tts-server/  # Feature specification and design docs
```

## Common Commands

```bash
# Build
cargo build                    # Debug build
cargo build --release          # Release build
cargo build --features all-providers  # Build with all TTS providers

# Test
cargo test                     # Run all tests
cargo test -p yappy-core       # Test specific crate
cargo test --test ws_basic     # Run specific integration test

# Lint & Format
cargo clippy                   # Run linter
cargo fmt                      # Format code
cargo fmt --check              # Check formatting

# Run
cargo run -p yappy-server      # Run server (debug)
cargo run -p yappy-server -- --help  # CLI help

# Development
just dev                       # Run with hot reload (requires just)
just test                      # Run tests
just lint                      # Run clippy + fmt check
```

## Feature Flags

| Flag | Description |
|------|-------------|
| `kokoro` | Kokoro 82M ONNX local model (default) |
| `openai-tts` | OpenAI TTS API |
| `avspeech` | macOS AVSpeechSynthesizer |
| `all-providers` | Enable all providers |

## Architecture Notes

- **Stateless streaming**: Text in → Audio out, no persistence
- **Provider abstraction**: `TtsProvider` trait in yappy-core
- **Sentence buffering**: Accumulates text, emits complete sentences
- **Backpressure**: Bounded channels pause TTS generation when client is slow
- **Binary audio frames**: WebSocket binary frames with 12-byte header

## API Endpoints

- `GET /health` - Health check (< 10ms)
- `GET /providers` - List TTS providers (< 50ms)
- `WS /ws` - WebSocket for streaming TTS

## Configuration

Server reads `yappy.toml` from working directory. See `specs/001-streaming-tts-server/contracts/config-schema.md`.

## Constitution Principles

This project follows principles defined in `.sdd/memory/constitution.md`:
- Unix philosophy (single purpose, composable)
- Fail fast with clear errors
- Privacy-aware (no logging of input text or API keys)
- Input validation at boundaries
- Integration tests over mocks

<!-- MANUAL ADDITIONS START -->
<!-- Add project-specific notes below this line -->
<!-- MANUAL ADDITIONS END -->

---
*Last updated: 2026-02-03 by /sdd:plan*
