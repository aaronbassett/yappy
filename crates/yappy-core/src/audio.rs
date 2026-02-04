//! Audio format and chunk types

use bytes::Bytes;
use futures::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

use crate::error::ProviderError;

/// Audio output format specification
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioFormat {
    /// Codec (opus, pcm, mp3)
    #[serde(default)]
    pub codec: AudioCodec,

    /// Sample rate in Hz (e.g., 24000, 48000)
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,

    /// Number of channels (1 = mono, 2 = stereo)
    #[serde(default = "default_channels")]
    pub channels: u8,

    /// Bits per sample (for PCM)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bits_per_sample: Option<u8>,
}

const fn default_sample_rate() -> u32 {
    48000
}

const fn default_channels() -> u8 {
    1
}

impl Default for AudioFormat {
    fn default() -> Self {
        Self {
            codec: AudioCodec::Opus,
            sample_rate: 48000,
            channels: 1,
            bits_per_sample: None,
        }
    }
}

/// Audio codec
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioCodec {
    /// Opus codec (default, good compression)
    #[default]
    Opus,
    /// Raw PCM samples
    Pcm,
    /// MP3 codec (maximum compatibility)
    Mp3,
}

impl std::fmt::Display for AudioCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Opus => write!(f, "opus"),
            Self::Pcm => write!(f, "pcm"),
            Self::Mp3 => write!(f, "mp3"),
        }
    }
}

/// A chunk of encoded audio
#[derive(Debug, Clone)]
pub struct AudioChunk {
    /// Sequence number within session
    pub sequence: u32,

    /// Which sentence this audio belongs to
    pub sentence_index: u32,

    /// Encoded audio data
    pub data: Bytes,

    /// Duration of this chunk in milliseconds
    pub duration_ms: u32,
}

impl AudioChunk {
    /// Create a new audio chunk
    pub const fn new(sequence: u32, sentence_index: u32, data: Bytes, duration_ms: u32) -> Self {
        Self {
            sequence,
            sentence_index,
            data,
            duration_ms,
        }
    }

    /// Serialize to binary WebSocket frame format
    ///
    /// Format: `[sequence:u32][sentence_index:u32][duration_ms:u32][data...]`
    pub fn to_binary_frame(&self) -> Bytes {
        let mut buf = Vec::with_capacity(12 + self.data.len());
        buf.extend_from_slice(&self.sequence.to_le_bytes());
        buf.extend_from_slice(&self.sentence_index.to_le_bytes());
        buf.extend_from_slice(&self.duration_ms.to_le_bytes());
        buf.extend_from_slice(&self.data);
        Bytes::from(buf)
    }

    /// Parse from binary WebSocket frame format
    pub fn from_binary_frame(data: &Bytes) -> Option<Self> {
        if data.len() < 12 {
            return None;
        }

        let sequence = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let sentence_index = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let duration_ms = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        let audio_data = data.slice(12..);

        Some(Self {
            sequence,
            sentence_index,
            data: audio_data,
            duration_ms,
        })
    }
}

/// Stream of audio chunks from provider
pub type AudioStream = Pin<Box<dyn Stream<Item = Result<AudioChunk, ProviderError>> + Send>>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that `to_binary_frame()` produces correct 12-byte header + data
    #[test]
    fn test_audio_chunk_to_binary_frame() {
        let audio_data = Bytes::from_static(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let chunk = AudioChunk::new(42, 7, audio_data.clone(), 100);

        let frame = chunk.to_binary_frame();

        // Total length should be 12-byte header + 4-byte data
        assert_eq!(frame.len(), 16);

        // Verify sequence (little-endian u32)
        assert_eq!(&frame[0..4], &42u32.to_le_bytes());

        // Verify sentence_index (little-endian u32)
        assert_eq!(&frame[4..8], &7u32.to_le_bytes());

        // Verify duration_ms (little-endian u32)
        assert_eq!(&frame[8..12], &100u32.to_le_bytes());

        // Verify audio data follows the header
        assert_eq!(&frame[12..], &audio_data[..]);
    }

    /// Test that `from_binary_frame()` correctly parses and recovers original values
    #[test]
    fn test_audio_chunk_from_binary_frame() {
        // Manually construct a binary frame
        let mut frame_data = Vec::new();
        frame_data.extend_from_slice(&123u32.to_le_bytes()); // sequence
        frame_data.extend_from_slice(&5u32.to_le_bytes()); // sentence_index
        frame_data.extend_from_slice(&250u32.to_le_bytes()); // duration_ms
        frame_data.extend_from_slice(&[0x01, 0x02, 0x03]); // audio data

        let frame = Bytes::from(frame_data);
        let chunk = AudioChunk::from_binary_frame(&frame).expect("should parse valid frame");

        assert_eq!(chunk.sequence, 123);
        assert_eq!(chunk.sentence_index, 5);
        assert_eq!(chunk.duration_ms, 250);
        assert_eq!(&chunk.data[..], &[0x01, 0x02, 0x03]);
    }

    /// Test that serializing then parsing returns the same values
    #[test]
    fn test_audio_chunk_roundtrip() {
        let original = AudioChunk::new(
            999,
            42,
            Bytes::from_static(&[0xCA, 0xFE, 0xBA, 0xBE, 0x00, 0xFF]),
            5000,
        );

        let frame = original.to_binary_frame();
        let parsed = AudioChunk::from_binary_frame(&frame).expect("roundtrip should succeed");

        assert_eq!(parsed.sequence, original.sequence);
        assert_eq!(parsed.sentence_index, original.sentence_index);
        assert_eq!(parsed.duration_ms, original.duration_ms);
        assert_eq!(parsed.data, original.data);
    }

    /// Test that `from_binary_frame()` returns None for data shorter than 12 bytes
    #[test]
    fn test_audio_chunk_from_binary_frame_too_short() {
        // Empty data
        let empty = Bytes::new();
        assert!(AudioChunk::from_binary_frame(&empty).is_none());

        // 11 bytes (one short of minimum header)
        let too_short = Bytes::from_static(&[0; 11]);
        assert!(AudioChunk::from_binary_frame(&too_short).is_none());

        // Exactly 12 bytes (valid - header only, no audio data)
        let just_header = Bytes::from_static(&[0; 12]);
        assert!(AudioChunk::from_binary_frame(&just_header).is_some());
    }

    /// Test that `AudioFormat` can be deserialized with only codec specified (protocol contract)
    #[test]
    fn test_audio_format_partial_codec_only() {
        let json = r#"{"codec": "opus"}"#;
        let format: AudioFormat = serde_json::from_str(json).unwrap();
        assert_eq!(format.codec, AudioCodec::Opus);
        assert_eq!(format.sample_rate, 48000); // default
        assert_eq!(format.channels, 1); // default
    }

    /// Test that `AudioFormat` can be deserialized with only `sample_rate` specified
    #[test]
    fn test_audio_format_partial_sample_rate_only() {
        let json = r#"{"sample_rate": 24000}"#;
        let format: AudioFormat = serde_json::from_str(json).unwrap();
        assert_eq!(format.codec, AudioCodec::Opus); // default
        assert_eq!(format.sample_rate, 24000);
        assert_eq!(format.channels, 1); // default
    }

    /// Test that empty `AudioFormat` object deserializes with all defaults
    #[test]
    fn test_audio_format_empty_object() {
        let json = "{}";
        let format: AudioFormat = serde_json::from_str(json).unwrap();
        assert_eq!(format.codec, AudioCodec::Opus);
        assert_eq!(format.sample_rate, 48000);
        assert_eq!(format.channels, 1);
    }

    /// Test that `AudioFormat` works with mixed specified and default fields
    #[test]
    fn test_audio_format_partial_mixed() {
        let json = r#"{"codec": "mp3", "channels": 2}"#;
        let format: AudioFormat = serde_json::from_str(json).unwrap();
        assert_eq!(format.codec, AudioCodec::Mp3);
        assert_eq!(format.sample_rate, 48000); // default
        assert_eq!(format.channels, 2);
    }
}
