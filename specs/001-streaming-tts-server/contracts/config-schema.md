# Configuration Schema

**File**: `yappy.toml`
**Format**: TOML

## Example Configuration

```toml
# Yappy TTS Server Configuration

[server]
host = "127.0.0.1"
port = 3000
log_level = "info"
idle_timeout_secs = 300      # 5 minutes
synthesis_timeout_secs = 30

[buffer]
flush_timeout_ms = 500
max_size_bytes = 4096

[providers]
default = "kokoro"

[providers.kokoro]
model_path = "auto"          # Downloads from HuggingFace on first use
# model_path = "/path/to/kokoro-v1.0.onnx"  # Or explicit path
voices_path = "auto"

[providers.openai]
api_key = "$OPENAI_API_KEY"  # Environment variable reference
model = "tts-1"              # or "tts-1-hd"

# macOS only - automatically enabled when available
[providers.avspeech]
enabled = true
```

## Schema Reference

### [server]

Server configuration.

| Key | Type | Required | Default | Description |
|-----|------|----------|---------|-------------|
| `host` | string | No | `"127.0.0.1"` | Bind address. Use `"0.0.0.0"` for all interfaces. |
| `port` | integer | No | `3000` | Port number (1-65535) |
| `log_level` | string | No | `"info"` | Log level: `"error"`, `"warn"`, `"info"`, `"debug"`, `"trace"` |
| `idle_timeout_secs` | integer | No | `300` | Idle WebSocket timeout in seconds |
| `synthesis_timeout_secs` | integer | No | `30` | Per-sentence synthesis timeout |

### [buffer]

Sentence buffer configuration.

| Key | Type | Required | Default | Description |
|-----|------|----------|---------|-------------|
| `flush_timeout_ms` | integer | No | `500` | Flush timeout when no sentence boundary detected |
| `max_size_bytes` | integer | No | `4096` | Maximum buffer size before forced flush |

### [providers]

Provider configuration.

| Key | Type | Required | Default | Description |
|-----|------|----------|---------|-------------|
| `default` | string | Yes | - | Default provider ID when client doesn't specify |

### [providers.kokoro]

Kokoro ONNX provider settings.

| Key | Type | Required | Default | Description |
|-----|------|----------|---------|-------------|
| `model_path` | string | No | `"auto"` | Path to ONNX model or `"auto"` for HF download |
| `voices_path` | string | No | `"auto"` | Path to voices file or `"auto"` for HF download |

### [providers.openai]

OpenAI TTS provider settings.

| Key | Type | Required | Default | Description |
|-----|------|----------|---------|-------------|
| `api_key` | string | Yes | - | API key. Supports `$ENV_VAR` syntax. |
| `model` | string | No | `"tts-1"` | Model: `"tts-1"` (faster) or `"tts-1-hd"` (quality) |

### [providers.avspeech]

macOS AVSpeechSynthesizer settings.

| Key | Type | Required | Default | Description |
|-----|------|----------|---------|-------------|
| `enabled` | boolean | No | `true` | Enable provider (macOS only) |

## Environment Variable References

API keys and sensitive values support environment variable references using `$VARNAME` syntax:

```toml
[providers.openai]
api_key = "$OPENAI_API_KEY"
```

The server expands these at startup. If the variable is not set, the provider will be marked as `not_configured`.

## Security Notes

1. **Default bind address**: Server binds to `127.0.0.1` by default. Binding to `0.0.0.0` requires explicit configuration.

2. **File permissions**: Configuration file should be readable only by the owner (mode `0600`). Server warns if file has group/world read permissions.

3. **API key handling**:
   - API keys are never logged
   - Use environment variables for production deployments
   - Errors do not include API key values

## Minimal Configuration

The minimal valid configuration requires only a default provider:

```toml
[providers]
default = "kokoro"

[providers.kokoro]
model_path = "auto"
```

All other values use sensible defaults.

## Platform-Specific Behavior

### macOS

- `avspeech` provider is automatically available
- Model files cached in `~/Library/Caches/yappy/`

### Linux

- `avspeech` provider is not available (macOS only)
- Model files cached in `~/.cache/yappy/` (or `$XDG_CACHE_HOME/yappy/`)

## Validation Rules

1. `server.port` must be 1-65535
2. `server.log_level` must be one of: error, warn, info, debug, trace
3. `providers.default` must reference a compiled provider
4. At least one provider must be configured and available
5. Paths must be valid filesystem paths or `"auto"`
