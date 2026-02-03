# Quickstart: Yappy TTS Server

## Prerequisites

- **Rust**: 1.75+ (2021 edition)
- **Platform**: macOS (Apple Silicon) or Linux (x86_64)

## Quick Start

```bash
# Clone and build
git clone https://github.com/yourusername/yappy.git
cd yappy
cargo build --release

# Create minimal config
cat > yappy.toml << 'EOF'
[providers]
default = "kokoro"

[providers.kokoro]
model_path = "auto"
EOF

# Run server (downloads model on first run)
./target/release/yappy-server

# Test with curl (in another terminal)
curl http://localhost:3000/health
```

## Build Options

### Default Build (Kokoro only)

```bash
cargo build --release
```

### All Providers

```bash
cargo build --release --features all-providers
```

### Specific Providers

```bash
# OpenAI TTS
cargo build --release --features openai-tts

# macOS AVSpeechSynthesizer
cargo build --release --features avspeech

# Multiple providers
cargo build --release --features "kokoro,openai-tts"
```

## Configuration

Create `yappy.toml` in the working directory:

```toml
[server]
host = "127.0.0.1"
port = 3000
log_level = "info"

[buffer]
flush_timeout_ms = 500
max_size_bytes = 4096

[providers]
default = "kokoro"

[providers.kokoro]
model_path = "auto"    # Downloads from HuggingFace

[providers.openai]
api_key = "$OPENAI_API_KEY"
```

See [contracts/config-schema.md](./contracts/config-schema.md) for full schema.

## API Endpoints

### Health Check

```bash
curl http://localhost:3000/health
```

### List Providers

```bash
curl http://localhost:3000/providers
```

### WebSocket TTS

```bash
# Using websocat (install: cargo install websocat)
websocat ws://localhost:3000/ws

# Send session init
{"type":"session.init","provider":"kokoro"}

# Send text
{"type":"text","content":"Hello, world!"}

# Signal end of input
{"type":"text.done"}
```

## Development Setup

```bash
# Install development tools
cargo install just lefthook

# Set up git hooks
lefthook install

# Run development server with hot reload
just dev

# Run tests
just test

# Run lints
just lint

# Format code
just fmt
```

## Testing

### Unit Tests

```bash
cargo test
```

### Integration Tests

```bash
cargo test --test '*'
```

### Manual WebSocket Test

```python
#!/usr/bin/env python3
"""Simple WebSocket test client"""
import asyncio
import websockets
import json

async def test_tts():
    async with websockets.connect("ws://localhost:3000/ws") as ws:
        # Initialize session
        await ws.send(json.dumps({
            "type": "session.init",
            "provider": "kokoro",
            "voice": {"id": "af_bella"}
        }))

        # Wait for ready
        response = await ws.recv()
        print(f"Session: {response}")

        # Send text
        await ws.send(json.dumps({
            "type": "text",
            "content": "Hello! This is a test of the Yappy TTS server."
        }))

        # Signal done
        await ws.send(json.dumps({"type": "text.done"}))

        # Receive audio chunks
        while True:
            msg = await ws.recv()
            if isinstance(msg, bytes):
                print(f"Audio chunk: {len(msg)} bytes")
            else:
                data = json.loads(msg)
                print(f"Message: {data}")
                if data.get("type") == "audio.done":
                    break

asyncio.run(test_tts())
```

## Project Structure

```
yappy/
├── Cargo.toml              # Workspace manifest
├── yappy.toml              # Server configuration
├── justfile                # Development tasks
├── lefthook.yml            # Git hooks
│
├── crates/
│   ├── yappy-core/         # Shared types and traits
│   ├── yappy-server/       # Axum server binary
│   ├── yappy-provider-kokoro/
│   ├── yappy-provider-openai/
│   └── yappy-provider-avspeech/
│
├── tests/
│   └── integration/
│
└── specs/
    └── 001-streaming-tts-server/
        ├── spec.md
        ├── plan.md
        ├── research.md
        ├── data-model.md
        ├── quickstart.md       # This file
        └── contracts/
```

## Common Issues

### Model Download Fails

```
Error: Failed to download model from HuggingFace
```

**Solution**: Check network connectivity. Optionally download manually:

```bash
# Download model files
curl -L -o ~/.cache/yappy/kokoro-v1.0.onnx \
  https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX/resolve/main/kokoro-v1.0.onnx

# Update config to use local path
[providers.kokoro]
model_path = "~/.cache/yappy/kokoro-v1.0.onnx"
```

### OpenAI API Key Not Set

```
Error: Provider 'openai' not configured: API key not set
```

**Solution**: Set the environment variable:

```bash
export OPENAI_API_KEY="sk-..."
```

### Port Already in Use

```
Error: Address already in use (os error 98)
```

**Solution**: Change port in config or stop existing process:

```bash
lsof -i :3000
kill <PID>
```

## Next Steps

- Read the [WebSocket Protocol](./contracts/websocket-protocol.md) for client integration
- Check [HTTP API](./contracts/http-api.md) for monitoring endpoints
- Review [Data Model](./data-model.md) for type definitions
