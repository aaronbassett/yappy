# Technical Research: Yappy Streaming TTS Server

**Date**: 2026-02-03
**Status**: Complete

## Summary of Decisions

| Question | Decision | Primary Crate(s) |
|----------|----------|------------------|
| Audio transcoding | Internal transcoding with PCM bypass option | `audiopus`, `mp3lame-encoder` |
| Sentence boundaries | SRX rules + punkt training | `srx`, `punkt` |
| Model distribution | Built-in HF Hub downloader | `hf-hub` |
| WebSocket backpressure | Bounded channels + cooperative pause | `tokio::sync::mpsc` |
| OpenAI TTS | Direct reqwest streaming | `reqwest` |
| macOS TTS | objc2 AVFoundation bindings | `objc2-avf-audio` |
| ONNX inference | ort primary, tract fallback | `ort`, `tract-onnx` |

---

## 1. Audio Transcoding

### Decision
**Internal transcoding with optional PCM bypass.** The server handles PCM-to-Opus and PCM-to-MP3 transcoding using `audiopus` and `mp3lame-encoder`. Provide raw PCM output option for clients needing other formats.

### Rationale

**Available Crates:**

| Crate | Purpose | Maturity | Notes |
|-------|---------|----------|-------|
| [audiopus](https://github.com/Lakelezz/audiopus) | Opus encoding | Stable (v0.2.0) | Thread-safe, cross-platform |
| [mp3lame-encoder](https://crates.io/crates/mp3lame-encoder) | MP3 encoding | Stable (v0.2.2) | Static LAME 3.100 build |
| [symphonia](https://github.com/pdeljanov/Symphonia) | Audio decoding | Stable | Decode-only, not encode |

**Key Points:**
- Real-time encoding achievable; TTS is the bottleneck, not encoding
- Both crates support static linking for single-binary distribution
- Opus frame sizes: 120, 240, 480, 960, 1920, 2880 samples at 48kHz

### Alternatives Rejected

1. **Reject unsupported formats**: Poor UX; clients need separate infrastructure
2. **FFmpeg FFI**: Massive dependency, complex build, licensing concerns
3. **Pure Rust only**: No production-ready pure Rust MP3 encoder exists

---

## 2. Sentence Boundary Detection

### Decision
**Use `srx` crate with custom rules as primary, `punkt` as fallback.** SRX provides rule-based segmentation with explicit abbreviation handling.

### Rationale

**Available Crates:**

| Crate | Approach | Abbreviation Handling | Decimal Numbers |
|-------|----------|----------------------|-----------------|
| [unicode-segmentation](https://crates.io/crates/unicode-segmentation) | UAX#29 | Poor - "Mr." triggers break | Poor |
| [srx](https://github.com/bminixhofer/srx) | Rule-based (SRX 2.0) | Excellent - explicit rules | Configurable |
| [punkt](https://github.com/ferristseng/rust-punkt) | Training-based | Good - learns from corpus | Moderate |

**SRX Implementation:**
- Implements Segmentation Rules eXchange 2.0 standard
- Can load LanguageTool's `segment.srx` for common abbreviations (Dr., Mrs., U.S.A., e.g., i.e.)
- Custom rule for decimals: `<rule break="no"><beforebreak>\d\.\d</beforebreak></rule>`

**Hybrid Approach:**
- SRX rules for known patterns (abbreviations, decimals)
- punkt training for domain adaptation if needed

### Alternatives Rejected

1. **unicode-segmentation only**: Fails on abbreviations
2. **Custom regex**: Brittle, hard to maintain
3. **External NLP service**: Adds latency and network dependency

---

## 3. Kokoro Model Distribution

### Decision
**Built-in downloader using `hf-hub` crate with lazy download on first use.** Cache models in HuggingFace-compatible location. Provide CLI command for pre-downloading.

### Rationale

**Model Files:**

| File | Size | Notes |
|------|------|-------|
| `kokoro-v1.0.onnx` (FP16) | ~170 MB | Recommended default |
| `voices-v1.0.bin` | ~27 MB | Voice embeddings |

**Distribution Approaches:**

| Approach | UX | Disk | Complexity |
|----------|-----|------|------------|
| Bundled in binary | Excellent | Always 200MB+ | Simple |
| curl/wget script | Manual step | On-demand | Low |
| Built-in downloader | Good (auto) | On-demand | Medium |

**hf-hub Features:**
- Sync and async (tokio) APIs
- Progress bar support
- Cache compatible with Python `huggingface_hub`

**Implementation Pattern:**
```rust
use hf_hub::api::tokio::Api;

async fn ensure_model() -> Result<PathBuf> {
    let api = Api::new()?;
    let repo = api.model("onnx-community/Kokoro-82M-v1.0-ONNX".to_string());
    let model_path = repo.get("kokoro-v1.0.onnx").await?;
    Ok(model_path)
}
```

### Alternatives Rejected

1. **Embed in binary**: Bloats binary; prevents model updates
2. **Require manual download**: Poor UX; support burden
3. **Git LFS**: Bloats repository; complicates cloning

---

## 4. WebSocket Backpressure

### Decision
**Bounded tokio::mpsc channels with cooperative producer pausing.** Three-tier strategy: bounded buffer, send timeout, graceful degradation.

### Rationale

**Axum WebSocket Defaults:**
- `write_buffer_size`: 128 KiB
- `max_write_buffer_size`: Unlimited (dangerous)

**Recommended Architecture:**
```
[TTS Engine] --> [Bounded Channel (32-64 frames)] --> [WebSocket Sender]
                         |
                  Backpressure signal
                         |
                  [Pause generation]
```

**Implementation Strategy:**

1. **Primary**: `tokio::sync::mpsc::channel(32)` - blocks when full
2. **Secondary**: `send_timeout(100ms)` for time-sensitive operations
3. **Tertiary**: Track "lagging" state, notify client of degradation

**Axum-Specific:**
- Configure `max_write_buffer_size()` on `WebSocketUpgrade`
- Use `ConcurrencyLimit` + `LoadShed` middleware for request throttling

### Alternatives Rejected

1. **Unbounded channels**: Memory unsafe; can OOM
2. **Ring buffer (drop oldest)**: Audio gaps more noticeable than video
3. **Client-side pull**: More complex protocol

---

## 5. Provider Implementations

### 5.1 OpenAI TTS API

**Decision**: Direct reqwest streaming implementation.

**Implementation:**
```rust
use reqwest::Client;
use futures::StreamExt;

async fn stream_openai_tts(text: &str, api_key: &str) -> impl Stream<Item = Bytes> {
    let client = Client::new();
    let response = client
        .post("https://api.openai.com/v1/audio/speech")
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&json!({
            "model": "tts-1",
            "input": text,
            "voice": "alloy",
            "response_format": "opus"
        }))
        .send()
        .await?;

    response.bytes_stream()
}
```

**Notes:**
- OpenAI TTS natively outputs Opus/MP3/PCM - no server transcoding needed
- Rate limiting: implement exponential backoff (FR-033)

### 5.2 macOS AVSpeechSynthesizer

**Decision**: Use `objc2-avf-audio` crate for safe Rust bindings.

**Crate**: [objc2-avf-audio](https://crates.io/crates/objc2-avf-audio) (v0.3.2)

**Key Types:**
- `AVSpeechSynthesizer`: Main synthesizer
- `AVSpeechSynthesisVoice`: Voice selection
- `AVSpeechUtterance`: Text + rate/pitch/volume
- Delegate protocols for progress callbacks

**Requirements:**
- macOS only (`#[cfg(target_os = "macos")]`)
- Use `AVSpeechSynthesizerBufferCallback` for streaming (macOS 10.15+)
- Outputs PCM buffers - server transcodes to Opus/MP3

### 5.3 ONNX Inference (Kokoro)

**Decision**: Use `ort` as primary, `tract` as pure-Rust fallback.

**Crate Comparison:**

| Crate | Type | Performance | Hardware Accel |
|-------|------|-------------|----------------|
| [ort](https://github.com/pykeio/ort) | FFI wrapper | Excellent | CUDA, CoreML, etc. |
| [tract](https://lib.rs/crates/tract-onnx) | Pure Rust | Good (~1.0-1.5x slower) | CPU only |

**ort Usage:**
```rust
use ort::{Session, inputs};

let session = Session::builder()?
    .with_execution_providers([CoreMLExecutionProvider::default().build()])?
    .commit_from_file("kokoro-v1.0.onnx")?;

let outputs = session.run(inputs![
    "input_ids" => input_tensor,
    "speaker_id" => speaker_tensor,
]?)?;
```

**tract Fallback:**
- Enable via feature flag for WASM or constrained environments
- `ort::set_api(ort_tract::api())` at startup

---

## Dependencies Summary

### Core Dependencies (Cargo.toml)

```toml
# Web/API
axum = { version = "0.8", features = ["ws"] }
tokio = { version = "1", features = ["full"] }
tower = "0.5"

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"

# Logging
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# Audio
audiopus = "0.2"
mp3lame-encoder = "0.2"

# Text Processing
srx = "0.1"
punkt = "0.2"

# Error Handling
thiserror = "1"
anyhow = "1"

# CLI
clap = { version = "4", features = ["derive"] }
```

### Provider Dependencies (feature-gated)

```toml
# OpenAI
reqwest = { version = "0.12", features = ["stream", "json"], optional = true }

# Kokoro
ort = { version = "2", optional = true }
hf-hub = { version = "0.4", features = ["tokio"], optional = true }

# macOS AVSpeech
objc2-avf-audio = { version = "0.3", optional = true }
```

---

## References

- [audiopus](https://github.com/Lakelezz/audiopus) - Rust Opus bindings
- [mp3lame-encoder](https://crates.io/crates/mp3lame-encoder) - LAME MP3 encoder
- [srx](https://github.com/bminixhofer/srx) - SRX 2.0 sentence segmentation
- [punkt](https://github.com/ferristseng/rust-punkt) - Punkt sentence tokenizer
- [hf-hub](https://github.com/huggingface/hf-hub) - HuggingFace Rust client
- [Kokoro-82M-v1.0-ONNX](https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX)
- [ort](https://github.com/pykeio/ort) - ONNX Runtime for Rust
- [tract-onnx](https://lib.rs/crates/tract-onnx) - Pure Rust ONNX
- [objc2-avf-audio](https://crates.io/crates/objc2-avf-audio) - AVFoundation bindings
- [Axum WebSocket](https://docs.rs/axum/latest/axum/extract/ws/)
