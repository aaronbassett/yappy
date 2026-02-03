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
    pub codec: AudioCodec,

    /// Sample rate in Hz (e.g., 24000, 48000)
    pub sample_rate: u32,

    /// Number of channels (1 = mono, 2 = stereo)
    pub channels: u8,

    /// Bits per sample (for PCM)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bits_per_sample: Option<u8>,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioCodec {
    /// Opus codec (default, good compression)
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
