# Tasks: Yappy Streaming TTS Server

**Input**: Design documents from `/specs/001-streaming-tts-server/`
**Prerequisites**: plan.md (required), spec.md (required), research.md, data-model.md, contracts/
**Generated**: 2026-02-03

**Tests**: Integration tests are included as part of the implementation (per constitution principle "Test What Matters").

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- **[GIT]**: Git workflow task (commit, push, PR)
- Include exact file paths in descriptions

## Path Conventions

- **Cargo workspace**: `crates/` at repository root
- **Server binary**: `crates/yappy-server/src/`
- **Core library**: `crates/yappy-core/src/`
- **Providers**: `crates/yappy-provider-*/src/`
- **Integration tests**: `tests/integration/`

---

## Phase 1: Setup (Project Initialization)

**Purpose**: Verify project structure and initialize development environment

### Git Start
- [x] T001 [GIT] Verify on feature branch 001-streaming-tts-server and working tree is clean
- [x] T002 [GIT] Pull and rebase on origin/main if needed

### Implementation
- [x] T003 Verify project structure matches plan.md (crates/, tests/, .github/) (use devs:rust-dev agent)
- [x] T004 [GIT] Commit: verify project structure
- [x] T005 [P] Verify Cargo.toml workspace configuration and dependencies (use devs:rust-dev agent)
- [x] T006 [P] Verify lefthook.yml pre-commit hooks configuration
- [x] T007 [P] Verify justfile development tasks
- [x] T008 [GIT] Commit: verify development tooling
- [x] T009 Verify .github/workflows/ci.yml and release.yml exist (use devs:rust-dev agent)
- [x] T010 [GIT] Commit: verify CI workflows

### Phase Completion
- [x] T011 [GIT] Push branch to origin (ensure pre-push hooks pass)
- [x] T012 [GIT] Create/update PR to main with phase summary
- [x] T013 [GIT] Verify all CI checks pass
- [x] T014 [GIT] Report PR ready status

---

## Phase 2: Foundational (Core Types & Infrastructure)

**Purpose**: Core infrastructure that MUST be complete before ANY user story can be implemented

**CRITICAL**: No user story work can begin until this phase is complete

### Phase Start
- [x] T015 [GIT] Verify working tree is clean before starting Phase 2
- [x] T016 [GIT] Pull and rebase on origin/main if needed
- [x] T017 Create specs/001-streaming-tts-server/retro/P2.md for this phase

### Core Types (yappy-core)
- [x] T018 [GIT] Commit: initialize phase 2 retro
- [x] T019 [P] Implement error types (ProviderError, SessionError) in crates/yappy-core/src/error.rs (pre-existing from Phase 1)
- [x] T020 [P] Implement AudioCodec, AudioFormat, AudioChunk types in crates/yappy-core/src/audio.rs (pre-existing from Phase 1)
- [x] T021 [P] Implement VoiceInfo, VoiceConfig, VoiceGender types in crates/yappy-core/src/voice.rs (pre-existing in provider.rs/session.rs from Phase 1)
- [x] T022 [GIT] Commit: add core audio and voice types (pre-existing from Phase 1)
- [x] T023 Implement TtsProvider trait and ProviderMetadata in crates/yappy-core/src/provider.rs (pre-existing from Phase 1)
- [x] T024 [GIT] Commit: add TtsProvider trait (pre-existing from Phase 1)
- [x] T025 [P] Implement SessionId, Session, SessionState types in crates/yappy-core/src/session.rs (pre-existing from Phase 1)
- [x] T026 [P] Implement CodeBlockMode enum in crates/yappy-core/src/session.rs (pre-existing from Phase 1)
- [x] T026a Implement SessionState lifecycle transitions (Init→Streaming→Complete→Cleanup) with state validation in crates/yappy-core/src/session.rs (pre-existing from Phase 1)
- [x] T027 [GIT] Commit: add session types (pre-existing from Phase 1)
- [x] T028 Implement BufferConfig and SentenceBuffer in crates/yappy-core/src/buffer.rs (pre-existing from Phase 1)
- [x] T029 [GIT] Commit: add sentence buffer (pre-existing from Phase 1)
- [x] T030 [P] Implement Config, ServerConfig, ProvidersConfig in crates/yappy-core/src/config.rs (pre-existing from Phase 1)
- [x] T031 [P] Implement environment variable expansion for config secrets in crates/yappy-core/src/config.rs (pre-existing from Phase 1)
- [x] T032 [GIT] Commit: add configuration types (pre-existing from Phase 1)
- [x] T033 Update crates/yappy-core/src/lib.rs to export all public types (pre-existing from Phase 1)
- [x] T034 [GIT] Commit: export all yappy-core types (pre-existing from Phase 1)

### WebSocket Message Types (yappy-core)
- [x] T035 [P] Implement ClientMessage enum (session.init, text, text.done) in crates/yappy-core/src/message.rs (use devs:rust-dev agent)
- [x] T036 [P] Implement ServerMessage enum (session.ready, audio.done, error, session.error) in crates/yappy-core/src/message.rs (use devs:rust-dev agent)
- [x] T037 [GIT] Commit: add WebSocket message types

### Server Infrastructure (yappy-server)
- [x] T038 Implement AppState with provider registry in crates/yappy-server/src/state.rs (use devs:rust-dev agent)
- [x] T039 [GIT] Commit: add AppState
- [x] T040 Implement Axum router setup in crates/yappy-server/src/app.rs (use devs:rust-dev agent)
- [ ] T041 [GIT] Commit: add Axum router
- [ ] T042 Implement CLI argument parsing in crates/yappy-server/src/main.rs (use devs:rust-dev agent)
- [ ] T043 [GIT] Commit: add CLI parsing
- [ ] T044a Implement config file TOML parsing and environment variable expansion in crates/yappy-server/src/main.rs (use devs:rust-dev agent)
- [ ] T044b Implement config validation (provider compatibility, required fields, value ranges) in crates/yappy-server/src/main.rs (use devs:rust-dev agent)
- [ ] T045 [GIT] Commit: add config loading and validation
- [ ] T046 Implement tracing/logging setup in crates/yappy-server/src/main.rs (use devs:rust-dev agent)
- [ ] T047 [GIT] Commit: add tracing setup

### Phase End
- [ ] T048 Run /sdd:map incremental for Phase 2 changes
- [ ] T049 [GIT] Commit: update codebase documents for phase 2
- [ ] T050 Review specs/001-streaming-tts-server/retro/P2.md and extract critical learnings to CLAUDE.md (conservative)
- [ ] T051 [GIT] Commit: finalize phase 2 retro

### Phase Completion
- [ ] T052 [GIT] Push branch to origin (ensure pre-push hooks pass)
- [ ] T053 [GIT] Create/update PR to main with phase summary
- [ ] T054 [GIT] Verify all CI checks pass
- [ ] T055 [GIT] Report PR ready status

**Checkpoint**: Foundation ready - Core types and server infrastructure complete

---

## Phase 3: User Story 1 - Basic Text-to-Speech Session (Priority: P1)

**Goal**: Streaming text in, streaming audio out via WebSocket

**Independent Test**: Connect via WebSocket, send text chunks, receive audio chunks, verify audio is playable

### Phase Start
- [ ] T056 [GIT] Verify working tree is clean before starting Phase 3
- [ ] T057 [GIT] Pull and rebase on origin/main if needed
- [ ] T058 [US1] Create specs/001-streaming-tts-server/retro/P3.md for this phase
- [ ] T059 [GIT] Commit: initialize phase 3 retro

### WebSocket Handler Implementation
- [ ] T060 [US1] Implement WebSocket upgrade handler in crates/yappy-server/src/ws.rs (use devs:rust-dev agent)
- [ ] T061 [GIT] Commit: add WebSocket upgrade handler
- [ ] T062 [US1] Implement session.init message handling in crates/yappy-server/src/ws.rs (use devs:rust-dev agent)
- [ ] T063 [GIT] Commit: add session.init handling
- [ ] T064 [US1] Implement text message handling with sentence buffer integration in crates/yappy-server/src/ws.rs (use devs:rust-dev agent)
- [ ] T065 [GIT] Commit: add text message handling
- [ ] T066 [US1] Implement text.done handling and buffer flush in crates/yappy-server/src/ws.rs (use devs:rust-dev agent)
- [ ] T067 [GIT] Commit: add text.done handling

### Audio Streaming
- [ ] T068 [US1] Implement binary audio frame serialization (12-byte header + data) in crates/yappy-core/src/audio.rs (use devs:rust-dev agent)
- [ ] T069 [GIT] Commit: add binary audio frame serialization
- [ ] T070 [US1] Implement audio chunk streaming from provider to WebSocket in crates/yappy-server/src/ws.rs (use devs:rust-dev agent)
- [ ] T071 [GIT] Commit: add audio streaming
- [ ] T072 [US1] Implement audio.done message with statistics in crates/yappy-server/src/ws.rs (use devs:rust-dev agent)
- [ ] T073 [GIT] Commit: add audio.done message

### Kokoro Provider (Default)
- [ ] T074 [US1] Implement Kokoro TtsProvider skeleton in crates/yappy-provider-kokoro/src/lib.rs (use devs:rust-dev agent)
- [ ] T075 [GIT] Commit: add Kokoro provider skeleton
- [ ] T076 [US1] Implement ONNX model loading with hf-hub in crates/yappy-provider-kokoro/src/lib.rs (use devs:rust-dev agent)
- [ ] T077 [GIT] Commit: add ONNX model loading
- [ ] T078 [US1] Implement Kokoro synthesize() method with ort inference in crates/yappy-provider-kokoro/src/lib.rs (use devs:rust-dev agent)
- [ ] T079 [GIT] Commit: add Kokoro synthesize
- [ ] T080 [US1] Implement Kokoro metadata() and health_check() in crates/yappy-provider-kokoro/src/lib.rs (use devs:rust-dev agent)
- [ ] T081 [GIT] Commit: add Kokoro metadata and health check

### Integration Test
- [ ] T082 [US1] Create WebSocket integration test for basic TTS flow in tests/integration/ws_basic.rs (use devs:rust-dev agent)
- [ ] T083 [GIT] Commit: add basic WebSocket integration test

### Phase End
- [ ] T084 [US1] Run /sdd:map incremental for Phase 3 changes
- [ ] T085 [GIT] Commit: update codebase documents for phase 3
- [ ] T086 [US1] Review specs/001-streaming-tts-server/retro/P3.md and extract critical learnings to CLAUDE.md (conservative)
- [ ] T087 [GIT] Commit: finalize phase 3 retro

### Phase Completion
- [ ] T088 [GIT] Push branch to origin (ensure pre-push hooks pass)
- [ ] T089 [GIT] Create/update PR to main with phase summary
- [ ] T090 [GIT] Verify all CI checks pass
- [ ] T091 [GIT] Report PR ready status

**Checkpoint**: MVP complete - Basic streaming TTS works end-to-end

---

## Phase 4: User Story 2 - Provider Discovery and Selection (Priority: P1)

**Goal**: List available providers and select per-session

**Independent Test**: Call /providers endpoint, verify response lists all compiled providers, then create session with specific provider

### Phase Start
- [ ] T092 [GIT] Verify working tree is clean before starting Phase 4
- [ ] T093 [GIT] Pull and rebase on origin/main if needed
- [ ] T094 [US2] Create specs/001-streaming-tts-server/retro/P4.md for this phase
- [ ] T095 [GIT] Commit: initialize phase 4 retro

### Provider Registry
- [ ] T096 [US2] Implement ProviderRegistry with feature-flag based provider registration in crates/yappy-server/src/state.rs (use devs:rust-dev agent)
- [ ] T097 [GIT] Commit: add ProviderRegistry
- [ ] T098 [US2] Implement provider status tracking (available/not_configured/unavailable) in crates/yappy-server/src/state.rs (use devs:rust-dev agent)
- [ ] T099 [GIT] Commit: add provider status tracking

### HTTP Endpoints
- [ ] T100 [US2] Implement GET /providers endpoint in crates/yappy-server/src/handlers/providers.rs (use devs:rust-dev agent)
  - **Note**: Blocks until T081 (Kokoro provider) complete - needs at least one provider registered
- [ ] T101 [GIT] Commit: add /providers endpoint
- [ ] T102 [US2] Implement provider selection in session.init handling in crates/yappy-server/src/ws.rs (use devs:rust-dev agent)
- [ ] T103 [GIT] Commit: add provider selection in session.init
- [ ] T104 [US2] Implement session.error for unavailable provider with alternatives in crates/yappy-server/src/ws.rs (use devs:rust-dev agent)
- [ ] T105 [GIT] Commit: add session.error for unavailable provider

### Integration Test
- [ ] T106 [US2] Create provider selection integration test in tests/integration/ws_providers.rs (use devs:rust-dev agent)
  - Must test: provider selection, provider unavailable fallback with alternatives, and FR-025 "no silent fallback" behavior
- [ ] T107 [GIT] Commit: add provider selection integration test

### Phase End
- [ ] T108 [US2] Run /sdd:map incremental for Phase 4 changes
- [ ] T109 [GIT] Commit: update codebase documents for phase 4
- [ ] T110 [US2] Review specs/001-streaming-tts-server/retro/P4.md and extract critical learnings to CLAUDE.md (conservative)
- [ ] T111 [GIT] Commit: finalize phase 4 retro

### Phase Completion
- [ ] T112 [GIT] Push branch to origin (ensure pre-push hooks pass)
- [ ] T113 [GIT] Create/update PR to main with phase summary
- [ ] T114 [GIT] Verify all CI checks pass
- [ ] T115 [GIT] Report PR ready status

**Checkpoint**: Provider discovery and selection working

---

## Phase 5: User Story 3 - Voice Selection and Configuration (Priority: P2)

**Goal**: Select specific voice and configure synthesis options per session

**Independent Test**: Create session with specific voice and custom speed, verify audio output reflects settings

### Phase Start
- [ ] T116 [GIT] Verify working tree is clean before starting Phase 5
- [ ] T117 [GIT] Pull and rebase on origin/main if needed
- [ ] T118 [US3] Create specs/001-streaming-tts-server/retro/P5.md for this phase
- [ ] T119 [GIT] Commit: initialize phase 5 retro

### Voice Configuration
- [ ] T120 [US3] Implement voice validation in session.init (check against provider's voice list) in crates/yappy-server/src/ws.rs (use devs:rust-dev agent)
- [ ] T121 [GIT] Commit: add voice validation
- [ ] T122 [US3] Implement invalid_voice error with alternatives in crates/yappy-server/src/ws.rs (use devs:rust-dev agent)
- [ ] T123 [GIT] Commit: add invalid_voice error
- [ ] T124 [US3] Implement speed/pitch/volume parameter passing to provider in crates/yappy-server/src/ws.rs (use devs:rust-dev agent)
- [ ] T125 [GIT] Commit: add voice parameter passing
- [ ] T126 [US3] Implement Kokoro voice list (af_bella, am_adam, etc.) in crates/yappy-provider-kokoro/src/lib.rs (use devs:rust-dev agent)
- [ ] T127 [GIT] Commit: add Kokoro voice list

### Integration Test
- [ ] T128 [US3] Create voice configuration integration test in tests/integration/ws_voices.rs (use devs:rust-dev agent)
- [ ] T129 [GIT] Commit: add voice configuration integration test

### Phase End
- [ ] T130 [US3] Run /sdd:map incremental for Phase 5 changes
- [ ] T131 [GIT] Commit: update codebase documents for phase 5
- [ ] T132 [US3] Review specs/001-streaming-tts-server/retro/P5.md and extract critical learnings to CLAUDE.md (conservative)
- [ ] T133 [GIT] Commit: finalize phase 5 retro

### Phase Completion
- [ ] T134 [GIT] Push branch to origin (ensure pre-push hooks pass)
- [ ] T135 [GIT] Create/update PR to main with phase summary
- [ ] T136 [GIT] Verify all CI checks pass
- [ ] T137 [GIT] Report PR ready status

**Checkpoint**: Voice selection and configuration working

---

## Phase 6: User Story 4 - Health Monitoring (Priority: P2)

**Goal**: Monitor server health and provider availability

**Independent Test**: Call GET /health, verify response includes server status, uptime, and per-provider status

### Phase Start
- [ ] T138 [GIT] Verify working tree is clean before starting Phase 6
- [ ] T139 [GIT] Pull and rebase on origin/main if needed
- [ ] T140 [US4] Create specs/001-streaming-tts-server/retro/P6.md for this phase
- [ ] T141 [GIT] Commit: initialize phase 6 retro

### Health Endpoint
- [ ] T142 [US4] Implement GET /health endpoint in crates/yappy-server/src/handlers/health.rs (use devs:rust-dev agent)
- [ ] T143 [GIT] Commit: add /health endpoint
- [ ] T144 [US4] Implement uptime tracking in AppState in crates/yappy-server/src/state.rs (use devs:rust-dev agent)
- [ ] T145 [GIT] Commit: add uptime tracking
- [ ] T146 [US4] Implement per-provider health_check() calls in health endpoint in crates/yappy-server/src/handlers/health.rs (use devs:rust-dev agent)
- [ ] T147 [GIT] Commit: add per-provider health checks
- [ ] T148 [US4] Implement degraded/unhealthy status logic in crates/yappy-server/src/handlers/health.rs (use devs:rust-dev agent)
- [ ] T149 [GIT] Commit: add health status logic

### Integration Test
- [ ] T150 [US4] Create health check integration test in tests/integration/health_check.rs (use devs:rust-dev agent)
- [ ] T151 [GIT] Commit: add health check integration test

### Phase End
- [ ] T152 [US4] Run /sdd:map incremental for Phase 6 changes
- [ ] T153 [GIT] Commit: update codebase documents for phase 6
- [ ] T154 [US4] Review specs/001-streaming-tts-server/retro/P6.md and extract critical learnings to CLAUDE.md (conservative)
- [ ] T155 [GIT] Commit: finalize phase 6 retro

### Phase Completion
- [ ] T156 [GIT] Push branch to origin (ensure pre-push hooks pass)
- [ ] T157 [GIT] Create/update PR to main with phase summary
- [ ] T158 [GIT] Verify all CI checks pass
- [ ] T159 [GIT] Report PR ready status

**Checkpoint**: Health monitoring working

---

## Phase 7: User Story 5 - Audio Format Negotiation (Priority: P2)

**Goal**: Request specific audio format (Opus/PCM/MP3) during session setup

**Independent Test**: Create sessions requesting different formats, verify audio chunks are encoded correctly

### Phase Start
- [ ] T160 [GIT] Verify working tree is clean before starting Phase 7
- [ ] T161 [GIT] Pull and rebase on origin/main if needed
- [ ] T162 [US5] Create specs/001-streaming-tts-server/retro/P7.md for this phase
- [ ] T163 [GIT] Commit: initialize phase 7 retro

### Audio Transcoding
- [ ] T164 [P] [US5] Implement Opus encoder wrapper using audiopus in crates/yappy-core/src/transcode.rs (use devs:rust-dev agent)
- [ ] T165 [P] [US5] Implement MP3 encoder wrapper using mp3lame-encoder in crates/yappy-core/src/transcode.rs (use devs:rust-dev agent)
- [ ] T166 [GIT] Commit: add audio encoders
- [ ] T167 [US5] Implement format negotiation in session.init (check provider support, transcode if needed) in crates/yappy-server/src/ws.rs (use devs:rust-dev agent)
- [ ] T168 [GIT] Commit: add format negotiation
- [ ] T169 [US5] Implement unsupported format error (invalid_format) in crates/yappy-server/src/ws.rs (use devs:rust-dev agent)
- [ ] T170 [GIT] Commit: add invalid_format error

### Integration Test
- [ ] T171 [US5] Create audio format integration test in tests/integration/ws_formats.rs (use devs:rust-dev agent)
- [ ] T172 [GIT] Commit: add audio format integration test

### Phase End
- [ ] T173 [US5] Run /sdd:map incremental for Phase 7 changes
- [ ] T174 [GIT] Commit: update codebase documents for phase 7
- [ ] T175 [US5] Review specs/001-streaming-tts-server/retro/P7.md and extract critical learnings to CLAUDE.md (conservative)
- [ ] T176 [GIT] Commit: finalize phase 7 retro

### Phase Completion
- [ ] T177 [GIT] Push branch to origin (ensure pre-push hooks pass)
- [ ] T178 [GIT] Create/update PR to main with phase summary
- [ ] T179 [GIT] Verify all CI checks pass
- [ ] T180 [GIT] Report PR ready status

**Checkpoint**: Audio format negotiation working

---

## Phase 8: User Story 6 - Error Recovery During Streaming (Priority: P2)

**Goal**: Handle non-fatal synthesis errors gracefully, continue streaming

**Independent Test**: Send text that triggers provider error mid-stream, verify error message received and subsequent sentences continue

### Phase Start
- [ ] T181 [GIT] Verify working tree is clean before starting Phase 8
- [ ] T182 [GIT] Pull and rebase on origin/main if needed
- [ ] T183 [US6] Create specs/001-streaming-tts-server/retro/P8.md for this phase
- [ ] T184 [GIT] Commit: initialize phase 8 retro

### Error Handling
- [ ] T185 [US6] Implement non-fatal error handling (synthesis_failed, rate_limited, provider_timeout) in crates/yappy-server/src/ws.rs (use devs:rust-dev agent)
- [ ] T186 [GIT] Commit: add non-fatal error handling
- [ ] T187 [US6] Implement sentence_index tracking for error correlation in crates/yappy-server/src/ws.rs (use devs:rust-dev agent)
- [ ] T188 [GIT] Commit: add sentence_index tracking
- [ ] T189 [US6] Implement fatal error handling with clean WebSocket close in crates/yappy-server/src/ws.rs (use devs:rust-dev agent)
- [ ] T190 [GIT] Commit: add fatal error handling

### Integration Test
- [ ] T191 [US6] Create error recovery integration test in tests/integration/ws_errors.rs (use devs:rust-dev agent)
- [ ] T192 [GIT] Commit: add error recovery integration test

### Phase End
- [ ] T193 [US6] Run /sdd:map incremental for Phase 8 changes
- [ ] T194 [GIT] Commit: update codebase documents for phase 8
- [ ] T195 [US6] Review specs/001-streaming-tts-server/retro/P8.md and extract critical learnings to CLAUDE.md (conservative)
- [ ] T196 [GIT] Commit: finalize phase 8 retro

### Phase Completion
- [ ] T197 [GIT] Push branch to origin (ensure pre-push hooks pass)
- [ ] T198 [GIT] Create/update PR to main with phase summary
- [ ] T199 [GIT] Verify all CI checks pass
- [ ] T200 [GIT] Report PR ready status

**Checkpoint**: Error recovery working

---

## Phase 9: User Story 7 - Sentence Buffer Intelligence (Priority: P3)

**Goal**: Intelligent text accumulation and sentence segmentation

**Independent Test**: Stream text with abbreviations (Dr., Mrs.), decimals (3.14), ellipses - verify no premature breaks

### Phase Start
- [ ] T201 [GIT] Verify working tree is clean before starting Phase 9
- [ ] T202 [GIT] Pull and rebase on origin/main if needed
- [ ] T203 [US7] Create specs/001-streaming-tts-server/retro/P9.md for this phase
- [ ] T204 [GIT] Commit: initialize phase 9 retro

### Sentence Buffer Enhancement
- [ ] T205 [US7] Implement SRX-based sentence segmentation with srx crate in crates/yappy-core/src/buffer.rs (use devs:rust-dev agent)
- [ ] T206 [GIT] Commit: add SRX segmentation
- [ ] T207 [US7] Add custom SRX rules for abbreviations (Dr., Mrs., U.S.A., e.g., i.e.) in crates/yappy-core/src/buffer.rs (use devs:rust-dev agent)
- [ ] T208 [GIT] Commit: add abbreviation rules
- [ ] T209 [US7] Add custom SRX rules for decimal numbers (3.14, 2.5) in crates/yappy-core/src/buffer.rs (use devs:rust-dev agent)
- [ ] T210 [GIT] Commit: add decimal rules
- [ ] T211 [US7] Implement flush timeout logic in crates/yappy-core/src/buffer.rs (use devs:rust-dev agent)
- [ ] T212 [GIT] Commit: add flush timeout
- [ ] T213 [US7] Implement max buffer overflow with clause/word boundary split in crates/yappy-core/src/buffer.rs (use devs:rust-dev agent)
- [ ] T214 [GIT] Commit: add buffer overflow handling
- [ ] T215 [US7] Implement code block detection and handling (skip, read_literally modes) in crates/yappy-core/src/buffer.rs (use devs:rust-dev agent)
- [ ] T215a [US7] Implement announce-and-skip code block mode with synthetic "code block" text emission in crates/yappy-core/src/buffer.rs (use devs:rust-dev agent)
- [ ] T216 [GIT] Commit: add code block handling

### Integration Test
- [ ] T217 [US7] Create sentence buffer integration test in tests/integration/ws_buffer.rs (use devs:rust-dev agent)
- [ ] T218 [GIT] Commit: add sentence buffer integration test

### Phase End
- [ ] T219 [US7] Run /sdd:map incremental for Phase 9 changes
- [ ] T220 [GIT] Commit: update codebase documents for phase 9
- [ ] T221 [US7] Review specs/001-streaming-tts-server/retro/P9.md and extract critical learnings to CLAUDE.md (conservative)
- [ ] T222 [GIT] Commit: finalize phase 9 retro

### Phase Completion
- [ ] T223 [GIT] Push branch to origin (ensure pre-push hooks pass)
- [ ] T224 [GIT] Create/update PR to main with phase summary
- [ ] T225 [GIT] Verify all CI checks pass
- [ ] T226 [GIT] Report PR ready status

**Checkpoint**: Intelligent sentence buffering working

---

## Phase 10: User Story 8 - Concurrent Session Support (Priority: P3)

**Goal**: Multiple simultaneous WebSocket connections without interference

**Independent Test**: Open 10 concurrent connections, stream different text, verify all complete correctly

### Phase Start
- [ ] T227 [GIT] Verify working tree is clean before starting Phase 10
- [ ] T228 [GIT] Pull and rebase on origin/main if needed
- [ ] T229 [US8] Create specs/001-streaming-tts-server/retro/P10.md for this phase
- [ ] T230 [GIT] Commit: initialize phase 10 retro

### Concurrency
- [ ] T231 [US8] Implement bounded channel backpressure (32-64 frames) in crates/yappy-server/src/ws.rs (use devs:rust-dev agent)
- [ ] T232 [GIT] Commit: add backpressure
- [ ] T233 [US8] Implement per-provider synthesis concurrency limit in crates/yappy-server/src/state.rs (use devs:rust-dev agent)
- [ ] T234 [GIT] Commit: add concurrency limit
- [ ] T235 [US8] Implement session isolation (no cross-contamination) verification in crates/yappy-server/src/ws.rs (use devs:rust-dev agent)
- [ ] T236 [GIT] Commit: add session isolation

### Integration Test
- [ ] T237 [US8] Create concurrent sessions integration test in tests/integration/ws_concurrent.rs (use devs:rust-dev agent)
- [ ] T237a [US8] Create backpressure behavior integration test verifying synthesis pauses/resumes with bounded channel in tests/integration/ws_backpressure.rs (use devs:rust-dev agent)
- [ ] T238 [GIT] Commit: add concurrent sessions and backpressure integration tests

### Phase End
- [ ] T239 [US8] Run /sdd:map incremental for Phase 10 changes
- [ ] T240 [GIT] Commit: update codebase documents for phase 10
- [ ] T241 [US8] Review specs/001-streaming-tts-server/retro/P10.md and extract critical learnings to CLAUDE.md (conservative)
- [ ] T242 [GIT] Commit: finalize phase 10 retro

### Phase Completion
- [ ] T243 [GIT] Push branch to origin (ensure pre-push hooks pass)
- [ ] T244 [GIT] Create/update PR to main with phase summary
- [ ] T245 [GIT] Verify all CI checks pass
- [ ] T246 [GIT] Report PR ready status

**Checkpoint**: Concurrent session support working

---

## Phase 11: User Story 9 - Graceful Shutdown (Priority: P3)

**Goal**: Complete in-progress synthesis on SIGTERM, close connections cleanly

**Independent Test**: Start streaming session, send SIGTERM, verify pending audio delivered and connection closes cleanly

### Phase Start
- [ ] T247 [GIT] Verify working tree is clean before starting Phase 11
- [ ] T248 [GIT] Pull and rebase on origin/main if needed
- [ ] T249 [US9] Create specs/001-streaming-tts-server/retro/P11.md for this phase
- [ ] T250 [GIT] Commit: initialize phase 11 retro

### Shutdown Handling
- [ ] T251 [US9] Implement SIGTERM signal handler with tokio in crates/yappy-server/src/main.rs (use devs:rust-dev agent)
- [ ] T252 [GIT] Commit: add signal handler
- [ ] T253 [US9] Implement graceful shutdown with connection drain (5s timeout) in crates/yappy-server/src/app.rs (use devs:rust-dev agent)
- [ ] T254 [GIT] Commit: add graceful shutdown
- [ ] T255 [US9] Implement provider cancellation token propagation in crates/yappy-server/src/ws.rs (use devs:rust-dev agent)
- [ ] T256 [GIT] Commit: add cancellation propagation

### Integration Test
- [ ] T257 [US9] Create graceful shutdown integration test in tests/integration/shutdown.rs (use devs:rust-dev agent)
- [ ] T258 [GIT] Commit: add graceful shutdown integration test

### Phase End
- [ ] T259 [US9] Run /sdd:map incremental for Phase 11 changes
- [ ] T260 [GIT] Commit: update codebase documents for phase 11
- [ ] T261 [US9] Review specs/001-streaming-tts-server/retro/P11.md and extract critical learnings to CLAUDE.md (conservative)
- [ ] T262 [GIT] Commit: finalize phase 11 retro

### Phase Completion
- [ ] T263 [GIT] Push branch to origin (ensure pre-push hooks pass)
- [ ] T264 [GIT] Create/update PR to main with phase summary
- [ ] T265 [GIT] Verify all CI checks pass
- [ ] T266 [GIT] Report PR ready status

**Checkpoint**: Graceful shutdown working

---

## Phase 12: Additional Providers (Optional)

**Goal**: Add OpenAI and AVSpeech providers

**Independent Test**: Create session with each provider, verify synthesis works

### Phase Start
- [ ] T267 [GIT] Verify working tree is clean before starting Phase 12
- [ ] T268 [GIT] Pull and rebase on origin/main if needed
- [ ] T269 Create specs/001-streaming-tts-server/retro/P12.md for this phase
- [ ] T270 [GIT] Commit: initialize phase 12 retro

### OpenAI Provider
- [ ] T271 Implement OpenAI TtsProvider in crates/yappy-provider-openai/src/lib.rs (use devs:rust-dev agent)
  - **Depends on**: Phase 3 (US1) and Phase 4 (US2) complete - requires TtsProvider trait and ProviderRegistry
- [ ] T272 [GIT] Commit: add OpenAI provider skeleton
- [ ] T273 Implement reqwest streaming for OpenAI TTS API in crates/yappy-provider-openai/src/lib.rs (use devs:rust-dev agent)
- [ ] T274 [GIT] Commit: add OpenAI streaming
- [ ] T275 Implement rate limit handling with exponential backoff in crates/yappy-provider-openai/src/lib.rs (use devs:rust-dev agent)
- [ ] T276 [GIT] Commit: add rate limit handling

### AVSpeech Provider (macOS only)
- [ ] T277 Implement AVSpeech TtsProvider with objc2-avf-audio in crates/yappy-provider-avspeech/src/lib.rs (use devs:rust-dev agent)
  - **Depends on**: Phase 3 (US1) and Phase 4 (US2) complete - requires TtsProvider trait and ProviderRegistry
- [ ] T278 [GIT] Commit: add AVSpeech provider skeleton
- [ ] T279 Implement AVSpeechSynthesizerBufferCallback for streaming in crates/yappy-provider-avspeech/src/lib.rs (use devs:rust-dev agent)
- [ ] T280 [GIT] Commit: add AVSpeech streaming
- [ ] T281 Implement system voice enumeration in crates/yappy-provider-avspeech/src/lib.rs (use devs:rust-dev agent)
- [ ] T282 [GIT] Commit: add AVSpeech voice enumeration

### Phase End
- [ ] T283 Run /sdd:map incremental for Phase 12 changes
- [ ] T284 [GIT] Commit: update codebase documents for phase 12
- [ ] T285 Review specs/001-streaming-tts-server/retro/P12.md and extract critical learnings to CLAUDE.md (conservative)
- [ ] T286 [GIT] Commit: finalize phase 12 retro

### Phase Completion
- [ ] T287 [GIT] Push branch to origin (ensure pre-push hooks pass)
- [ ] T288 [GIT] Create/update PR to main with phase summary
- [ ] T289 [GIT] Verify all CI checks pass
- [ ] T290 [GIT] Report PR ready status

**Checkpoint**: All providers implemented

---

## Phase 13: Polish & Cross-Cutting Concerns

**Purpose**: Improvements that affect multiple user stories

### Phase Start
- [ ] T291 [GIT] Verify working tree is clean before starting Phase 13
- [ ] T292 [GIT] Pull and rebase on origin/main if needed
- [ ] T293 Create specs/001-streaming-tts-server/retro/P13.md for this phase
- [ ] T294 [GIT] Commit: initialize phase 13 retro

### Timeouts
- [ ] T295 [P] Implement idle connection timeout (5 min) in crates/yappy-server/src/ws.rs (use devs:rust-dev agent)
- [ ] T296 [P] Implement synthesis timeout (30 sec) in crates/yappy-server/src/ws.rs (use devs:rust-dev agent)
- [ ] T297 [P] Implement session init timeout (10 sec) in crates/yappy-server/src/ws.rs (use devs:rust-dev agent)
- [ ] T298 [GIT] Commit: add timeouts

### Security
- [ ] T299 [P] Implement config file permission warning in crates/yappy-server/src/main.rs (use devs:rust-dev agent)
- [ ] T300 [P] Implement input text size validation (64KB max) in crates/yappy-server/src/ws.rs (use devs:rust-dev agent)
- [ ] T301 [GIT] Commit: add security validations

### Observability
- [ ] T302 [P] Add session ID to all tracing spans in crates/yappy-server/src/ws.rs (use devs:rust-dev agent)
- [ ] T303 [P] Add X-Request-ID header to HTTP responses in crates/yappy-server/src/app.rs (use devs:rust-dev agent)
- [ ] T304 [GIT] Commit: add observability improvements

### Validation
- [ ] T305 Run quickstart.md validation - verify all documented commands work
- [ ] T306 [GIT] Commit: quickstart validation complete
- [ ] T307 Run all integration tests with cargo test --test '*'
- [ ] T308 [GIT] Commit: all tests passing

### Phase End
- [ ] T309 Run /sdd:map incremental for Phase 13 changes
- [ ] T310 [GIT] Commit: update codebase documents for phase 13
- [ ] T311 Review specs/001-streaming-tts-server/retro/P13.md and extract critical learnings to CLAUDE.md (conservative)
- [ ] T312 [GIT] Commit: finalize phase 13 retro

### Phase Completion
- [ ] T313 [GIT] Push branch to origin (ensure pre-push hooks pass)
- [ ] T314 [GIT] Create/update PR to main with phase summary
- [ ] T315 [GIT] Verify all CI checks pass
- [ ] T316 [GIT] Report PR ready status

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies - can start immediately
- **Foundational (Phase 2)**: Depends on Setup - BLOCKS all user stories
- **User Story 1 (Phase 3)**: Depends on Foundational - Core streaming
- **User Story 2 (Phase 4)**: Depends on Foundational - Provider discovery (can parallel with US1)
- **User Story 3 (Phase 5)**: Depends on US1 complete - Voice selection
- **User Story 4 (Phase 6)**: Depends on US2 complete - Health monitoring
- **User Story 5 (Phase 7)**: Depends on US1 complete - Format negotiation
- **User Story 6 (Phase 8)**: Depends on US1 complete - Error recovery
- **User Story 7 (Phase 9)**: Depends on US1 complete - Sentence buffer
- **User Story 8 (Phase 10)**: Depends on US1 complete - Concurrency
- **User Story 9 (Phase 11)**: Depends on US1 complete - Graceful shutdown
- **Additional Providers (Phase 12)**: Depends on US1, US2 complete - Optional
- **Polish (Phase 13)**: Depends on all desired user stories complete

### User Story Dependencies

```
Phase 2 (Foundational)
    │
    ├─── Phase 3 (US1: Basic TTS) ──┬─── Phase 5 (US3: Voice Selection)
    │                               ├─── Phase 7 (US5: Audio Format)
    │                               ├─── Phase 8 (US6: Error Recovery)
    │                               ├─── Phase 9 (US7: Sentence Buffer)
    │                               ├─── Phase 10 (US8: Concurrency)
    │                               └─── Phase 11 (US9: Graceful Shutdown)
    │
    └─── Phase 4 (US2: Provider Discovery) ─── Phase 6 (US4: Health Monitoring)
                                           │
                                           └─── Phase 12 (Additional Providers)
```

### Parallel Opportunities

- Within Foundational: T019/T020/T021 (core types), T025/T026 (session), T030/T031 (config), T035/T036 (messages)
- Within each User Story: Tests and models marked [P]
- US1 and US2 can proceed in parallel after Foundational
- US3, US5, US6, US7, US8, US9 can all proceed in parallel after US1

---

## Parallel Example: Foundational Phase

```bash
# Launch all core type implementations together:
Task: "Implement error types in crates/yappy-core/src/error.rs"
Task: "Implement AudioCodec types in crates/yappy-core/src/audio.rs"
Task: "Implement VoiceInfo types in crates/yappy-core/src/voice.rs"

# Then launch config implementations together:
Task: "Implement Config types in crates/yappy-core/src/config.rs"
Task: "Implement env var expansion in crates/yappy-core/src/config.rs"
```

---

## Implementation Strategy

### MVP First (Phases 1-3)

1. Complete Phase 1: Setup
2. Complete Phase 2: Foundational
3. Complete Phase 3: User Story 1 (Basic TTS)
4. **STOP and VALIDATE**: Test end-to-end WebSocket TTS flow
5. Deploy/demo if ready

### Incremental Delivery

1. Setup + Foundational → Foundation ready
2. Add US1 (Basic TTS) → Test → Deploy (MVP!)
3. Add US2 (Provider Discovery) → Test → Deploy
4. Add US3, US5, US6, US7, US8, US9 → Test each → Deploy
5. Add US4 (Health) → Test → Deploy (Production ready!)
6. Add Additional Providers → Test → Deploy

### Parallel Team Strategy

With multiple developers:

1. Team completes Setup + Foundational together
2. Once Foundational is done:
   - Developer A: User Story 1 (Basic TTS)
   - Developer B: User Story 2 (Provider Discovery)
3. After US1/US2 complete:
   - Developer A: US3, US5, US7
   - Developer B: US4, US6, US8, US9

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story
- [GIT] tasks = git workflow (commit, push, PR)
- Each user story is independently completable and testable
- Commit after each logical task or group
- Stop at any checkpoint to validate independently
- Agents: All `.rs` file tasks use `devs:rust-dev` agent
