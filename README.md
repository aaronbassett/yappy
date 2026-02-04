# Yappy

[![CI](https://img.shields.io/github/actions/workflow/status/aaronbassett/yappy/ci.yml?style=flat-square)](https://github.com/aaronbassett/yappy/actions)
[![Rust](https://img.shields.io/badge/rust-1.75+-orange.svg?style=flat-square)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg?style=flat-square)](LICENSE)

**Give your AI assistant a voice.**

Yappy is a streaming text-to-speech server that accepts text input via WebSocket and produces near-realtime audio output. It features a pluggable provider system supporting local ONNX models, cloud APIs, and platform-native synthesis.

## Features

- **Streaming Architecture** - Text in, audio out with minimal latency. Sentence-level buffering enables audio playback while text is still arriving.
- **Multiple TTS Providers** - Choose between local inference (Kokoro), cloud APIs (OpenAI), or platform-native (macOS AVSpeech).
- **Audio Format Options** - Output as Opus, MP3, or raw PCM at configurable sample rates.
- **Client Agnostic** - Any WebSocket client works. No bundled UI, no opinions about your frontend.
- **Production Ready** - Health checks, graceful shutdown, backpressure handling, and structured logging.

## Quick Start

### Prerequisites

- Rust 1.75 or later
- For Kokoro provider: ~500MB disk space for model files (auto-downloaded)
- For OpenAI provider: OpenAI API key

### Installation

```bash
# Clone the repository
git clone https://github.com/aaronbassett/yappy.git
cd yappy

# Build with the default Kokoro provider
cargo build --release

# Or build with all providers
cargo build --release --features all-providers
```

### Running the Server

```bash
# Create a minimal configuration
cat > yappy.toml << 'EOF'
[providers]
default = "kokoro"

[providers.kokoro]
model_path = "auto"
EOF

# Run the server
./target/release/yappy-server

# Or run directly with cargo
cargo run -p yappy-server --release
```

The server starts on `http://127.0.0.1:3000` by default. On first run with Kokoro, model files are automatically downloaded from HuggingFace.

### Verify It Works

```bash
# Check server health
curl http://localhost:3000/health

# List available providers and voices
curl http://localhost:3000/providers
```

## Usage

### WebSocket Protocol

Connect to `ws://localhost:3000/ws` and follow this message flow:

```
Client                                    Server
  |                                         |
  |---- session.init --------------------->|
  |                                         |
  |<------------------------ session.ready  |
  |                                         |
  |---- text {"content": "Hello"} -------->|
  |                                         |
  |<-------------------- [Binary: audio]    |
  |<-------------------- [Binary: audio]    |
  |                                         |
  |---- text.done ------------------------>|
  |                                         |
  |<-------------------- [Binary: audio]    |
  |<--------------------------- audio.done  |
```

### Example: Python Client

```python
import asyncio
import json
import websockets

async def text_to_speech(text: str) -> bytes:
    audio_chunks = []

    async with websockets.connect("ws://localhost:3000/ws") as ws:
        # Initialize session
        await ws.send(json.dumps({
            "type": "session.init",
            "provider": "kokoro",
            "voice": {"id": "af_bella"},
            "audio_format": {"codec": "opus", "sample_rate": 48000}
        }))

        # Wait for ready
        response = json.loads(await ws.recv())
        assert response["type"] == "session.ready"

        # Send text
        await ws.send(json.dumps({
            "type": "text",
            "content": text
        }))

        # Signal end of text
        await ws.send(json.dumps({"type": "text.done"}))

        # Collect audio chunks
        while True:
            message = await ws.recv()
            if isinstance(message, bytes):
                # Binary frame: 12-byte header + audio data
                audio_chunks.append(message[12:])
            else:
                data = json.loads(message)
                if data["type"] == "audio.done":
                    break

    return b"".join(audio_chunks)

# Usage
audio = asyncio.run(text_to_speech("Hello, world!"))
```

### Example: JavaScript Client

```javascript
const ws = new WebSocket("ws://localhost:3000/ws");
const audioChunks = [];

ws.onopen = () => {
  ws.send(JSON.stringify({
    type: "session.init",
    provider: "kokoro",
    voice: { id: "af_bella" },
    audio_format: { codec: "opus", sample_rate: 48000 }
  }));
};

ws.onmessage = (event) => {
  if (event.data instanceof Blob) {
    // Binary audio data (skip 12-byte header)
    event.data.slice(12).arrayBuffer().then(buf => {
      audioChunks.push(new Uint8Array(buf));
    });
  } else {
    const msg = JSON.parse(event.data);
    if (msg.type === "session.ready") {
      ws.send(JSON.stringify({ type: "text", content: "Hello, world!" }));
      ws.send(JSON.stringify({ type: "text.done" }));
    } else if (msg.type === "audio.done") {
      console.log(`Received ${msg.total_bytes} bytes of audio`);
      ws.close();
    }
  }
};
```

## Configuration

Yappy reads configuration from `yappy.toml` in the working directory.

### Full Configuration Example

```toml
[server]
host = "127.0.0.1"          # Bind address (use "0.0.0.0" for all interfaces)
port = 3000                  # Port number
log_level = "info"           # error, warn, info, debug, trace
idle_timeout_secs = 300      # WebSocket idle timeout (5 minutes)
synthesis_timeout_secs = 30  # Per-sentence synthesis timeout

[buffer]
flush_timeout_ms = 500       # Flush incomplete sentences after this delay
max_size_bytes = 4096        # Force flush at this buffer size

[providers]
default = "kokoro"           # Default provider when client doesn't specify

[providers.kokoro]
model_path = "auto"          # "auto" downloads from HuggingFace
voices_path = "auto"         # Or specify explicit paths

[providers.openai]
api_key = "$OPENAI_API_KEY"  # Environment variable reference
model = "tts-1"              # "tts-1" (faster) or "tts-1-hd" (higher quality)

[providers.avspeech]
enabled = true               # macOS only
```

### Environment Variables

Sensitive values support `$VARNAME` syntax for environment variable expansion:

```toml
[providers.openai]
api_key = "$OPENAI_API_KEY"
```

### CLI Options

```
yappy-server [OPTIONS]

Options:
  -c, --config <PATH>    Path to configuration file [default: yappy.toml]
      --host <HOST>      Override bind address
  -p, --port <PORT>      Override port
      --log-level <LVL>  Log level (also: RUST_LOG env var)
  -h, --help             Print help
  -V, --version          Print version
```

## TTS Providers

### Kokoro (Default)

Local ONNX-based TTS using the Kokoro 82M model. No API keys required.

**Voices:**
| ID | Name | Language |
|----|------|----------|
| `af_bella` | Bella | American English (Female) |
| `am_adam` | Adam | American English (Male) |

**Audio Formats:** PCM (24kHz, 48kHz), Opus (48kHz), MP3 (24kHz, 48kHz)

```bash
# Build with Kokoro support (default)
cargo build --release --features kokoro
```

### OpenAI TTS

Cloud-based TTS using OpenAI's API. Requires an API key.

**Voices:** alloy, echo, fable, onyx, nova, shimmer

**Audio Formats:** Opus (48kHz), MP3 (24kHz), PCM (24kHz)

```bash
# Build with OpenAI support
cargo build --release --features openai-tts

# Configure
export OPENAI_API_KEY="sk-..."
```

### AVSpeech (macOS)

Platform-native synthesis using Apple's AVSpeechSynthesizer. macOS only.

```bash
# Build with AVSpeech support (macOS only)
cargo build --release --features avspeech
```

## API Reference

### HTTP Endpoints

#### GET /health

Health check for monitoring and load balancers.

```bash
curl http://localhost:3000/health
```

```json
{
  "status": "ok",
  "uptime_secs": 3600,
  "providers": {
    "kokoro": { "status": "available" },
    "openai": { "status": "not_configured", "reason": "API key not set" }
  }
}
```

Status values: `ok` (all providers available), `degraded` (some unavailable), `unhealthy` (none available).

#### GET /providers

List available providers with their voices and capabilities.

```bash
curl http://localhost:3000/providers
```

```json
{
  "providers": [
    {
      "id": "kokoro",
      "name": "Kokoro 82M",
      "status": "available",
      "voices": [
        { "id": "af_bella", "name": "Bella", "language": "en-US", "gender": "female" }
      ],
      "supported_formats": [
        { "codec": "opus", "sample_rates": [48000] },
        { "codec": "mp3", "sample_rates": [24000, 48000] }
      ]
    }
  ],
  "default_provider": "kokoro"
}
```

### WebSocket Messages

#### Client to Server

| Message | Description |
|---------|-------------|
| `session.init` | Initialize TTS session with provider, voice, and format options |
| `text` | Send text chunk for synthesis |
| `text.done` | Signal end of text input |

#### Server to Client

| Message | Description |
|---------|-------------|
| `session.ready` | Session initialized, ready for text |
| `[Binary]` | Audio chunk (12-byte header + encoded audio) |
| `audio.done` | All audio sent, includes summary stats |
| `error` | Non-fatal error (connection continues) |
| `session.error` | Fatal error (connection closes) |

#### Audio Binary Frame Format

```
+---------------+-----------------+--------------+---------------+
| sequence (4B) | sentence_idx(4B)| duration(4B) | audio data... |
+---------------+-----------------+--------------+---------------+
```

All integers are little-endian unsigned 32-bit.

## Project Structure

```
yappy/
├── crates/
│   ├── yappy-core/              # Shared types: TtsProvider trait, Session, AudioChunk
│   ├── yappy-server/            # Axum server binary and WebSocket handling
│   ├── yappy-provider-kokoro/   # Kokoro ONNX provider
│   ├── yappy-provider-openai/   # OpenAI TTS API provider
│   └── yappy-provider-avspeech/ # macOS AVSpeech provider
├── specs/                       # Feature specifications and contracts
├── yappy.toml                   # Configuration file
└── Cargo.toml                   # Workspace definition
```

## Development

### Building

```bash
# Debug build
cargo build

# Release build
cargo build --release

# Build with specific providers
cargo build --features kokoro,openai-tts

# Build with all providers
cargo build --features all-providers
```

### Testing

```bash
# Run all tests
cargo test

# Run tests for a specific crate
cargo test -p yappy-core

# Run integration tests
cargo test --test ws_basic
```

### Linting

```bash
# Run clippy
cargo clippy

# Format code
cargo fmt

# Check formatting
cargo fmt --check
```

### Feature Flags

| Flag | Description |
|------|-------------|
| `kokoro` | Kokoro 82M ONNX local model (default) |
| `openai-tts` | OpenAI TTS API |
| `avspeech` | macOS AVSpeechSynthesizer |
| `all-providers` | Enable all providers |

## Architecture

### Design Principles

- **Stateless Streaming** - No persistence, no session storage. Text flows in, audio flows out.
- **Provider Abstraction** - The `TtsProvider` trait in yappy-core allows pluggable backends.
- **Sentence Buffering** - Text accumulates until sentence boundaries are detected, then synthesized.
- **Backpressure** - Bounded channels pause TTS generation when the client can't keep up.

### Audio Pipeline

```
Text Input -> Sentence Buffer -> TTS Provider -> Audio Encoder -> WebSocket
```

1. **Sentence Buffer**: Accumulates incoming text, detects sentence boundaries using SRX rules
2. **TTS Provider**: Synthesizes complete sentences to raw PCM audio
3. **Audio Encoder**: Encodes to requested format (Opus, MP3, or passthrough PCM)
4. **WebSocket**: Streams binary frames with sequence headers to client

## Troubleshooting

### Model Download Fails

If automatic model download fails, manually download the Kokoro model:

```bash
# Create cache directory
mkdir -p ~/.cache/yappy

# Download model (example URL - check HuggingFace for current version)
wget -O ~/.cache/yappy/kokoro-v1.0.onnx \
  "https://huggingface.co/kokoro/kokoro-82m/resolve/main/kokoro-v1.0.onnx"

# Update config to use explicit path
echo 'model_path = "~/.cache/yappy/kokoro-v1.0.onnx"' >> yappy.toml
```

### OpenAI Provider Not Available

Ensure your API key is set:

```bash
export OPENAI_API_KEY="sk-..."
```

Or add to configuration:

```toml
[providers.openai]
api_key = "sk-..."  # Not recommended for production
```

### Connection Refused

Check that the server is running and the port is correct:

```bash
# Check if server is listening
ss -tlnp | grep 3000

# Try with explicit host
curl http://127.0.0.1:3000/health
```

### Audio Playback Issues

Ensure your audio player supports the codec you requested:

- **Opus**: Most modern players, browsers with `MediaSource`
- **MP3**: Universal support
- **PCM**: Raw samples, requires manual handling

## Performance

| Metric | Target | Notes |
|--------|--------|-------|
| Health check latency | < 10ms | No provider calls |
| Provider list latency | < 50ms | Cached capabilities |
| First audio chunk | < 500ms | Depends on text length and provider |
| Sentence synthesis | < 2s | Provider-dependent |

### Tuning

For lower latency:
```toml
[buffer]
flush_timeout_ms = 200    # Faster incomplete sentence flush
max_size_bytes = 2048     # Smaller buffer = more frequent synthesis
```

For higher throughput:
```toml
[buffer]
flush_timeout_ms = 1000   # Wait longer for complete sentences
max_size_bytes = 8192     # Larger batches
```

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## Acknowledgments

- [Kokoro](https://huggingface.co/kokoro) for the ONNX TTS model
- [Axum](https://github.com/tokio-rs/axum) for the web framework
- [ONNX Runtime](https://onnxruntime.ai/) for model inference
