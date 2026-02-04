# HTTP API Contract

**Base URL**: `http://localhost:3000`

## Endpoints

### GET /health

Health check endpoint for monitoring and load balancers.

**Response Time Target**: < 10ms

#### Success Response (200 OK)

```json
{
  "status": "ok",
  "uptime_secs": 3600,
  "providers": {
    "kokoro": {
      "status": "available"
    },
    "openai": {
      "status": "available"
    },
    "avspeech": {
      "status": "not_configured",
      "reason": "macOS only"
    }
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `status` | string | `"ok"` if any provider available, `"degraded"` if some unavailable, `"unhealthy"` if none available |
| `uptime_secs` | number | Seconds since server started |
| `providers` | object | Map of provider ID to status |
| `providers.{id}.status` | string | `"available"`, `"not_configured"`, or `"unavailable"` |
| `providers.{id}.reason` | string? | Human-readable reason (if not available) |

#### Unhealthy Response (503 Service Unavailable)

Returned when no providers are available.

```json
{
  "status": "unhealthy",
  "uptime_secs": 3600,
  "providers": {
    "kokoro": {
      "status": "unavailable",
      "reason": "Model file not found"
    },
    "openai": {
      "status": "not_configured",
      "reason": "API key not set"
    }
  }
}
```

---

### GET /providers

List all compiled providers with their capabilities.

**Response Time Target**: < 50ms

#### Success Response (200 OK)

```json
{
  "providers": [
    {
      "id": "kokoro",
      "name": "Kokoro 82M",
      "description": "Local ONNX-based TTS model with natural voices",
      "status": "available",
      "voices": [
        {
          "id": "af_bella",
          "name": "Bella (American Female)",
          "language": "en-US",
          "gender": "female"
        },
        {
          "id": "am_adam",
          "name": "Adam (American Male)",
          "language": "en-US",
          "gender": "male"
        }
      ],
      "supported_formats": [
        {"codec": "pcm", "sample_rates": [24000, 48000]},
        {"codec": "opus", "sample_rates": [48000]},
        {"codec": "mp3", "sample_rates": [24000, 48000]}
      ],
      "options_schema": {
        "type": "object",
        "properties": {
          "speed": {"type": "number", "minimum": 0.5, "maximum": 2.0},
          "pitch": {"type": "number", "minimum": -1.0, "maximum": 1.0}
        }
      }
    },
    {
      "id": "openai",
      "name": "OpenAI TTS",
      "description": "OpenAI's cloud-based text-to-speech API",
      "status": "available",
      "voices": [
        {"id": "alloy", "name": "Alloy", "language": "en-US", "gender": "neutral"},
        {"id": "echo", "name": "Echo", "language": "en-US", "gender": "male"},
        {"id": "fable", "name": "Fable", "language": "en-US", "gender": "female"},
        {"id": "onyx", "name": "Onyx", "language": "en-US", "gender": "male"},
        {"id": "nova", "name": "Nova", "language": "en-US", "gender": "female"},
        {"id": "shimmer", "name": "Shimmer", "language": "en-US", "gender": "female"}
      ],
      "supported_formats": [
        {"codec": "opus", "sample_rates": [48000]},
        {"codec": "mp3", "sample_rates": [24000]},
        {"codec": "pcm", "sample_rates": [24000]}
      ],
      "options_schema": {
        "type": "object",
        "properties": {
          "model": {"type": "string", "enum": ["tts-1", "tts-1-hd"]},
          "speed": {"type": "number", "minimum": 0.25, "maximum": 4.0}
        }
      }
    },
    {
      "id": "avspeech",
      "name": "AVSpeechSynthesizer",
      "description": "macOS built-in speech synthesis",
      "status": "not_configured",
      "reason": "Only available on macOS"
    }
  ],
  "default_provider": "kokoro"
}
```

#### Provider Object

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Unique provider identifier |
| `name` | string | Human-readable name |
| `description` | string | Provider description |
| `status` | string | `"available"`, `"not_configured"`, or `"unavailable"` |
| `reason` | string? | Why provider is not available |
| `voices` | array? | Available voices (if status is available) |
| `supported_formats` | array? | Supported audio formats |
| `options_schema` | object? | JSON Schema for provider-specific options |

#### Voice Object

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Voice identifier for use in session.init |
| `name` | string | Human-readable voice name |
| `language` | string | BCP 47 language code (e.g., "en-US") |
| `gender` | string? | `"male"`, `"female"`, or `"neutral"` |
| `sample_url` | string? | URL to sample audio (if available) |

#### Format Object

| Field | Type | Description |
|-------|------|-------------|
| `codec` | string | `"opus"`, `"pcm"`, or `"mp3"` |
| `sample_rates` | number[] | Supported sample rates in Hz |

---

## Common Headers

### Request Headers

| Header | Required | Description |
|--------|----------|-------------|
| `Accept` | No | `application/json` (default) |

### Response Headers

| Header | Value | Description |
|--------|-------|-------------|
| `Content-Type` | `application/json` | Response format |
| `X-Request-ID` | UUID | Request tracing ID |

---

## Error Responses

All errors follow this format:

```json
{
  "error": {
    "code": "not_found",
    "message": "The requested resource was not found"
  }
}
```

### HTTP Status Codes

| Status | Code | Description |
|--------|------|-------------|
| 400 | `bad_request` | Invalid request format |
| 404 | `not_found` | Resource not found |
| 500 | `internal_error` | Server error |
| 503 | `service_unavailable` | No providers available |
