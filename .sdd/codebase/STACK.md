# Technology Stack

**Status**: Planned (Greenfield Project)
**Last Updated**: 2026-02-03

## Core Language

- **Rust** (2021 edition) - Primary language for all server and library code
  - Type safety and memory safety without garbage collection
  - Async/await for concurrent operations
  - Excellent performance characteristics for streaming workloads

## Framework & Runtime

- **Axum** - Web framework for HTTP and WebSocket handling
  - Tower middleware ecosystem
  - Native async/await support
  - WebSocket upgrade support
- **Tokio** - Async runtime
  - Multi-threaded scheduler
  - Timer and channel primitives

## Project Structure

- **Cargo Workspace** - Multi-crate monorepo structure
  - `yappy-server` - Binary crate (Axum server, CLI)
  - `yappy-core` - Library crate (TtsProvider trait, types, utilities)
  - `yappy-provider-*` - Provider implementation crates

## Dependencies (Planned)

### Core
- `axum` - Web framework
- `tokio` - Async runtime
- `serde` / `serde_json` - Serialization
- `toml` - Configuration parsing
- `tracing` - Structured logging

### Audio Processing
- `opus` - Opus encoding (if transcoding required)
- Provider-specific: `kokoro-onnx`, Objective-C FFI (macOS), `reqwest` (OpenAI)

### Utilities
- `typeid` - Type-safe session IDs
- `thiserror` / `anyhow` - Error handling
- `clap` - CLI argument parsing

## Feature Flags

| Flag             | Description                         | Default |
|------------------|-------------------------------------|---------|
| `avspeech`       | AVSpeechSynthesizer (macOS only)    | Off     |
| `kokoro`         | Kokoro 82M ONNX local model         | Off     |
| `openai-tts`     | OpenAI TTS API                      | Off     |
| `all-providers`  | Enable all providers                | Off     |

## Build Targets

- **Primary**: macOS (Apple Silicon) - aarch64-apple-darwin
- **Secondary**: Linux (x86_64) - x86_64-unknown-linux-gnu

## Development Tools

- `cargo fmt` - Code formatting
- `cargo clippy` - Linting
- `cargo test` - Testing
- Conventional commits enforced
