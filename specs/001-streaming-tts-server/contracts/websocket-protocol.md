# WebSocket Protocol Contract

**Version**: 1.0.0
**Endpoint**: `ws://localhost:3000/ws`

## Overview

Bidirectional WebSocket connection for streaming text-to-speech. Client sends text messages (JSON), server responds with JSON control messages and binary audio frames.

## Connection Flow

```
Client                                    Server
   │                                         │
   │──── WebSocket Connect ─────────────────▶│
   │                                         │
   │◀─────────────────────── Connection Open │
   │                                         │
   │──── session.init ──────────────────────▶│
   │                                         │
   │◀──────────────────────── session.ready  │
   │                                         │
   │──── text {"content": "Hello"} ─────────▶│
   │                                         │
   │◀──────────────────── [Binary: audio]    │
   │◀──────────────────── [Binary: audio]    │
   │                                         │
   │──── text.done ─────────────────────────▶│
   │                                         │
   │◀──────────────────── [Binary: audio]    │
   │◀────────────────────────── audio.done   │
   │                                         │
   │──── Close ─────────────────────────────▶│
   └─────────────────────────────────────────┘
```

## Client → Server Messages

All client messages are JSON text frames.

### session.init

Initialize a new TTS session. Must be the first message sent.

```json
{
  "type": "session.init",
  "provider": "kokoro",
  "voice": {
    "id": "af_bella",
    "speed": 1.0,
    "pitch": 0.0,
    "volume": 1.0
  },
  "audio_format": {
    "codec": "opus",
    "sample_rate": 48000,
    "channels": 1
  },
  "code_block_mode": "skip"
}
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `type` | string | Yes | - | Must be `"session.init"` |
| `provider` | string | No | Server default | Provider ID from `/providers` |
| `voice` | object | No | Provider default | Voice configuration |
| `voice.id` | string | No | Provider default | Voice identifier |
| `voice.speed` | number | No | 1.0 | Speech rate (0.5 - 2.0) |
| `voice.pitch` | number | No | 0.0 | Pitch adjustment (-1.0 - 1.0) |
| `voice.volume` | number | No | 1.0 | Volume (0.0 - 1.0) |
| `audio_format` | object | No | Opus 48kHz mono | Audio format |
| `audio_format.codec` | string | No | "opus" | "opus", "pcm", or "mp3" |
| `audio_format.sample_rate` | number | No | 48000 | Sample rate in Hz |
| `audio_format.channels` | number | No | 1 | 1 (mono) or 2 (stereo) |
| `code_block_mode` | string | No | "skip" | "skip", "read_literally", "announce_and_skip" |

### text

Send text chunk for synthesis. May be sent multiple times.

```json
{
  "type": "text",
  "content": "Hello, world. This is a test."
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `type` | string | Yes | Must be `"text"` |
| `content` | string | Yes | UTF-8 text to synthesize (max 64KB) |

### text.done

Signal end of text input. Server will flush buffers and send remaining audio.

```json
{
  "type": "text.done"
}
```

## Server → Client Messages

### session.ready (JSON text frame)

Confirms session initialization. Sent in response to `session.init`.

```json
{
  "type": "session.ready",
  "session_id": "ses_01HQXYZ123ABC",
  "audio_format": {
    "codec": "opus",
    "sample_rate": 48000,
    "channels": 1
  },
  "voice": "af_bella"
}
```

### Audio Data (Binary frame)

Audio chunks are sent as binary WebSocket frames with a fixed header:

```
┌───────────────┬─────────────────┬──────────────┬───────────────┐
│ sequence (4B) │ sentence_idx(4B)│ duration(4B) │ audio data... │
└───────────────┴─────────────────┴──────────────┴───────────────┘
```

| Offset | Size | Type | Description |
|--------|------|------|-------------|
| 0 | 4 | u32 LE | Sequence number (starts at 0) |
| 4 | 4 | u32 LE | Sentence index this audio belongs to |
| 8 | 4 | u32 LE | Duration in milliseconds |
| 12 | N | bytes | Encoded audio data |

### audio.done (JSON text frame)

Signals all audio has been sent. Sent after `text.done` processing completes.

```json
{
  "type": "audio.done",
  "total_sentences": 5,
  "total_duration_ms": 12340,
  "total_bytes": 98765
}
```

### error (JSON text frame)

Non-fatal error during processing. Connection remains open.

```json
{
  "type": "error",
  "code": "synthesis_failed",
  "message": "Failed to synthesize sentence 3: provider timeout",
  "fatal": false,
  "sentence_index": 3
}
```

| Field | Type | Description |
|-------|------|-------------|
| `code` | string | Error code (see Error Codes) |
| `message` | string | Human-readable description |
| `fatal` | boolean | Always `false` for this message type |
| `sentence_index` | number? | Which sentence failed (if applicable) |

### session.error (JSON text frame)

Fatal error. Server will close connection after sending.

```json
{
  "type": "session.error",
  "code": "provider_unavailable",
  "message": "Provider 'openai' is not configured",
  "alternatives": ["kokoro", "avspeech"]
}
```

| Field | Type | Description |
|-------|------|-------------|
| `code` | string | Error code (see Error Codes) |
| `message` | string | Human-readable description |
| `alternatives` | string[]? | Available alternatives (for provider errors) |

## Error Codes

### Non-Fatal (connection continues)

| Code | Description |
|------|-------------|
| `synthesis_failed` | TTS provider failed to synthesize a sentence |
| `rate_limited` | Provider rate limit hit (will retry) |
| `provider_timeout` | Provider request timed out |

### Fatal (connection closes)

| Code | Description |
|------|-------------|
| `provider_unavailable` | Requested provider not available |
| `invalid_voice` | Voice ID not found for provider |
| `invalid_format` | Audio format not supported |
| `protocol_violation` | Invalid message or sequence |
| `internal_error` | Server internal error |

## Example Session

```
→ {"type":"session.init","provider":"kokoro","voice":{"id":"af_bella"}}
← {"type":"session.ready","session_id":"ses_01HQ...","audio_format":{"codec":"opus","sample_rate":48000,"channels":1},"voice":"af_bella"}
→ {"type":"text","content":"Hello, Dr. Smith. The value is 3.14."}
← [Binary: 12 bytes header + Opus audio data]
← [Binary: 12 bytes header + Opus audio data]
→ {"type":"text.done"}
← [Binary: 12 bytes header + Opus audio data]
← {"type":"audio.done","total_sentences":2,"total_duration_ms":2500,"total_bytes":12000}
```

## Timeouts

| Timeout | Default | Description |
|---------|---------|-------------|
| Idle | 5 min | No messages received |
| Synthesis | 30 sec | Per-sentence synthesis time |
| Session init | 10 sec | Time to send session.init after connect |
