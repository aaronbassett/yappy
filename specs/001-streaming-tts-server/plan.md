<!--
==============================================================================
PLAN TEMPLATE
==============================================================================

PURPOSE:
  Defines technical implementation plans with architecture decisions, file
  structure, and constitution compliance. Bridges specification (WHAT) to
  tasks (HOW).

WHEN USED:
  - By /sdd:plan command when creating implementation plans
  - After spec is created and approved
  - Sets technical context for the entire feature

CUSTOMIZATION:
  - Add project-specific technical context fields
  - Customize complexity tracking for your constitution
  - Add architecture decision sections relevant to your domain
  - Override by creating .sdd/templates/plan-template.md in your repo

LEARN MORE:
  See plugins/sdd/skills/sdd-infrastructure/references/template-guide.md
  for detailed documentation and examples.

==============================================================================
-->

# Implementation Plan: Yappy Streaming TTS Server

**Branch**: `001-streaming-tts-server` | **Date**: 2026-02-03 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `/specs/001-streaming-tts-server/spec.md`

## Summary

Yappy is a standalone streaming TTS server that accepts text via WebSocket and produces near-realtime audio output. The server is stateless and client-agnostic, featuring a pluggable provider system (AVSpeech/Kokoro/OpenAI) selected per-session, intelligent sentence buffering, and multiple audio format support (Opus/PCM/MP3).

## Technical Context

**Language/Version**: Rust 2021 edition (stable, latest)
**Primary Dependencies**: Axum (web/WebSocket), Tokio (async runtime), Serde (serialization), Tracing (logging)
**ONNX Runtime**: `ort` 2.0.0-rc.11 (RC accepted: only RC available with required features; monitor for stable release during implementation)
**Storage**: N/A (stateless - no persistence)
**Testing**: cargo test, integration tests for WebSocket flows
**Target Platform**: macOS (Apple Silicon) primary, Linux (x86_64) secondary
**Project Type**: Cargo workspace (multi-crate monorepo)
**Performance Goals**: First audio byte <500ms (local) / <1000ms (cloud), 10 concurrent sessions, /health <10ms, /providers <50ms
**Constraints**: Session setup <100ms (local) / <500ms (cloud), graceful shutdown <5s, 30s synthesis timeout, 5min idle timeout
**Scale/Scope**: Single server instance, 10+ concurrent sessions

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

### I. Unix Philosophy
- [x] **Single Purpose**: Server converts text streams to audio - one thing well
- [x] **Text I/O Protocol**: WebSocket text in → binary audio out, errors via structured messages
- [x] **Composable**: Any WebSocket client can connect; output is standard audio formats
- [x] **Exit Codes**: Server process uses: 0 = clean shutdown, 1 = configuration error, 2 = fatal runtime error

### II. Fail Fast & Loud
- [x] **Session errors include**: what failed, why, suggested fix (error codes + messages)
- [x] **Fatal vs non-fatal**: Protocol errors crash session; synthesis errors continue
- [x] **No silent failures**: All errors emit session.error messages

### III. Privacy-Aware Data Handling
- [x] **Never log input text**: FR-040 states text must not be logged beyond synthesis
- [x] **Never log secrets**: SC-010 requires zero sensitive data in logs
- [x] **Explicit consent**: Providers documented with data handling notes

**Privacy Logging Strategy** (implements Constitution III):
- Define `SensitiveString` newtype wrapper that implements `Debug` as `[REDACTED]`
- Use `#[serde(skip)]` on sensitive fields in all structs that may be logged
- Session state structs exclude: `input_text`, `api_key`, `provider_config` from Debug impl
- Tracing spans use explicit field lists, never `?session` or `?config` patterns
- Integration test verifies: debug-level logs of 100 sessions contain zero input text matches

### IV. Input Validation Required
- [x] **Validate input**: Session config validated at connection time
- [x] **Sanitize paths**: Config file paths validated
- [x] **Validate API responses**: Provider responses checked before processing

### V. Test What Matters
- [x] **Integration tests first**: WebSocket flow tests priority
- [x] **Test error paths**: Non-fatal/fatal error scenarios
- [x] **CLI contract**: N/A (server, not CLI)
- [x] **Property-based testing**: Sentence buffer edge cases

### VI. Human-Readable Output
- [x] **Progress indicators**: Audio streaming provides implicit progress
- [x] **Clear distinction**: stderr for logs, WebSocket for data
- [x] **JSON output**: All WebSocket messages are JSON (except binary audio)
- [x] **Error suggestions**: Error messages include codes and context

### VII. Modularity & Clean Boundaries
- [x] **Separate concerns**: yappy-server (CLI/API), yappy-core (traits/types), yappy-provider-* (implementations)
- [x] **Independently testable**: Each crate has its own tests
- [x] **Explicit dependencies**: Cargo workspace enforces DAG
- [x] **Library-first**: Core logic in yappy-core, usable without server

**Gate Status**: ✅ PASS - All principles addressed in design

## Project Structure

### Documentation (this feature)

```text
specs/001-streaming-tts-server/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output (WebSocket/HTTP API specs)
└── tasks.md             # /sdd:tasks command output
```

### Source Code (Cargo Workspace)

```text
Cargo.toml                      # Workspace manifest
yappy.toml                      # Server configuration (example)

crates/
├── yappy-core/                 # Library crate - shared types and traits
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── provider.rs         # TtsProvider trait
│       ├── session.rs          # Session types
│       ├── audio.rs            # AudioFormat, AudioChunk types
│       ├── buffer.rs           # SentenceBuffer implementation
│       ├── config.rs           # Configuration types
│       └── error.rs            # Error types
│
├── yappy-server/               # Binary crate - Axum server
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs             # Entry point, CLI
│       ├── app.rs              # Axum router setup
│       ├── ws.rs               # WebSocket handler
│       ├── handlers/
│       │   ├── mod.rs
│       │   ├── health.rs       # GET /health
│       │   └── providers.rs    # GET /providers
│       └── state.rs            # AppState (provider registry)
│
├── yappy-provider-avspeech/    # macOS AVSpeechSynthesizer (feature-gated)
│   ├── Cargo.toml
│   └── src/
│       └── lib.rs
│
├── yappy-provider-kokoro/      # Kokoro ONNX local model (feature-gated)
│   ├── Cargo.toml
│   └── src/
│       └── lib.rs
│
└── yappy-provider-openai/      # OpenAI TTS API (feature-gated)
    ├── Cargo.toml
    └── src/
        └── lib.rs

tests/
├── integration/
│   ├── ws_basic.rs             # Basic WebSocket flow
│   ├── ws_providers.rs         # Provider selection
│   └── health_check.rs         # HTTP endpoints
└── fixtures/
    └── test_config.toml

.github/
└── workflows/
    ├── ci.yml                  # Test + lint on PR
    └── release.yml             # Build release binaries

justfile                        # Development task runner
lefthook.yml                    # Pre-commit hooks config
```

**Structure Decision**: Cargo workspace with separate crates for core library, server binary, and provider implementations. This enables:
- Independent compilation and testing per crate
- Feature flags to include/exclude providers at build time
- Clear dependency graph (server → core → providers)

## Module Organization

### Error Hierarchy
```
YappyError (yappy-server)
├── ConfigError      - TOML parsing, env var expansion, validation
├── ProviderError    - Provider-specific failures (yappy-core)
│   ├── SynthesisError   - TTS generation failed
│   ├── RateLimitError   - Cloud provider throttling
│   └── TimeoutError     - Cancellation timeout exceeded
└── SessionError     - WebSocket/protocol errors (yappy-core)
    ├── ProtocolViolation - Invalid message format
    └── InitTimeout       - session.init not received in 10s
```

### Cancellation Token Flow
```
main.rs (SIGTERM handler)
    └── CancellationToken::cancel()
            └── AppState.shutdown_signal
                    └── ws.rs (per-session)
                            └── provider.synthesize(cancel_token)
                                    └── Provider checks token.is_cancelled()
```

### Binary Audio Frame Wire Format
Audio chunks are sent as WebSocket binary frames with this exact layout:
```
Offset  Size  Field           Type      Description
0       4     sequence        u32 LE    Frame sequence number (0-indexed)
4       4     sentence_index  u32 LE    Which sentence this audio belongs to
8       4     duration_ms     u32 LE    Duration of this chunk in milliseconds
12      N     audio_data      [u8]      Encoded audio bytes (Opus/PCM/MP3)
```
Total header: 12 bytes. `AudioChunk` Rust struct maps directly to this format via `bincode` serialization.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| Multiple crates (5+) | Provider isolation, feature flags | Single crate would couple all providers, bloating builds |
| WebSocket protocol | Bidirectional streaming required | HTTP would require polling, losing realtime capability |
| Backpressure channels | Prevent memory exhaustion under load | Unbounded queues risk OOM; drop semantics lose audio |

*Note: These are justified complexity, not violations of constitution principles.*

**Feature Flag Implementation** (TODO for Phase 2):
- Provider dependencies MUST be feature-gated in individual crate Cargo.toml files
- `ort`, `hf-hub` only in `yappy-provider-kokoro/Cargo.toml` under `[dependencies]` (not workspace)
- `reqwest` only in `yappy-provider-openai/Cargo.toml`
- `objc2-avf-audio` only in `yappy-provider-avspeech/Cargo.toml` with `[target.'cfg(target_os = "macos")'.dependencies]`
- Workspace Cargo.toml defines features: `kokoro`, `openai-tts`, `avspeech`, `all-providers`

## Open Questions (RESOLVED in Phase 0)

From spec "Open Questions for Implementation":

1. **Audio transcoding responsibility**: ✅ Server handles internal transcoding with PCM bypass option. Uses `audiopus` and `mp3lame-encoder`. Supported combinations: PCM→Opus, PCM→MP3. Unsupported combinations return session.error "invalid_format".

2. **Sentence buffer implementation**: ✅ Use `srx` crate with custom rules as primary. **Note**: SRX crate availability must be verified during Phase 2 implementation. If unavailable, fallback to `unicode-segmentation` with custom abbreviation handling (Dr., Mrs., etc. patterns as regex).

3. **Model file management for Kokoro**: ✅ Built-in downloader using `hf-hub` crate with lazy download on first use. Cache location: `~/.cache/yappy` (Linux), `~/Library/Caches/yappy` (macOS). **CLI pre-download is deferred** to future enhancement (out of scope for v1).

See [research.md](./research.md) for full technical analysis.

---

## Phase Completion Status

| Phase | Status | Output |
|-------|--------|--------|
| Pre-Phase: Learn from Retros | ✅ Complete | No previous retros (first feature) |
| Phase 0: Research | ✅ Complete | [research.md](./research.md) |
| Phase 1: Design & Contracts | ✅ Complete | [data-model.md](./data-model.md), [contracts/](./contracts/), [quickstart.md](./quickstart.md), CLAUDE.md |
| Phase 2: Dev Environment | ✅ Complete | Cargo workspace, lefthook, justfile, CI workflows |

---

## Generated Artifacts

### Phase 0: Research
- `specs/001-streaming-tts-server/research.md` - Technical decisions for all open questions

### Phase 1: Design & Contracts
- `specs/001-streaming-tts-server/data-model.md` - Entity definitions, Rust types
- `specs/001-streaming-tts-server/contracts/websocket-protocol.md` - WebSocket API spec
- `specs/001-streaming-tts-server/contracts/http-api.md` - REST API spec
- `specs/001-streaming-tts-server/contracts/config-schema.md` - Configuration schema
- `specs/001-streaming-tts-server/quickstart.md` - Developer quickstart guide
- `CLAUDE.md` - Agent context file

### Phase 2: Dev Environment
- `Cargo.toml` - Workspace manifest with all dependencies
- `rustfmt.toml` - Code formatting configuration
- `clippy.toml` - Linter configuration
- `lefthook.yml` - Git hooks (conventional commits, pre-commit, pre-push)
- `justfile` - Development task runner
- `.github/workflows/ci.yml` - CI pipeline (test, lint, build)
- `.github/workflows/release.yml` - Release pipeline (multi-platform binaries)
- `yappy.toml.example` - Example configuration file
- `crates/*/` - Skeleton crate structure with stub implementations

---

## Next Steps

Run `/sdd:tasks` to generate implementation tasks based on this plan.
