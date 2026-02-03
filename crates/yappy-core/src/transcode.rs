//! Audio transcoding module for converting between audio formats.
//!
//! This module provides encoder wrappers and a unified transcoding interface
//! for converting PCM audio data to compressed formats like Opus and MP3.
//!
//! # Supported Transcoding Paths
//!
//! - PCM -> Opus: Supported (via `audiopus`)
//! - PCM -> MP3: Supported (via `mp3lame-encoder`)
//! - PCM -> PCM: Pass-through (no conversion)
//! - Opus -> MP3: Not supported (requires decoding first)
//! - MP3 -> Opus: Not supported (requires decoding first)
//!
//! # Example
//!
//! ```no_run
//! use yappy_core::transcode::{Transcoder, TranscodeError};
//! use yappy_core::audio::{AudioFormat, AudioCodec};
//!
//! // Check if transcoding is possible
//! if Transcoder::can_transcode(AudioCodec::Pcm, AudioCodec::Opus) {
//!     let pcm_format = AudioFormat {
//!         codec: AudioCodec::Pcm,
//!         sample_rate: 24000,
//!         channels: 1,
//!         bits_per_sample: Some(16),
//!     };
//!     let opus_format = AudioFormat {
//!         codec: AudioCodec::Opus,
//!         sample_rate: 24000,
//!         channels: 1,
//!         bits_per_sample: None,
//!     };
//!
//!     // pcm_data would be i16 little-endian samples
//!     // let result = Transcoder::transcode(&pcm_data, &pcm_format, &opus_format);
//! }
//! ```

use crate::audio::{AudioCodec, AudioFormat};
use thiserror::Error;
use tracing::{debug, instrument};

/// Errors that can occur during transcoding operations.
#[derive(Debug, Error)]
pub enum TranscodeError {
    /// The requested transcoding path is not supported.
    #[error("Unsupported transcoding path: {from} -> {to}")]
    UnsupportedPath {
        /// Source codec.
        from: AudioCodec,
        /// Target codec.
        to: AudioCodec,
    },

    /// The input audio format is invalid or incompatible.
    #[error("Invalid input format: {reason}")]
    InvalidInputFormat {
        /// Description of the format issue.
        reason: String,
    },

    /// Encoder initialization failed.
    #[error("Encoder initialization failed: {message}")]
    EncoderInitFailed {
        /// Description of the initialization failure.
        message: String,
    },

    /// Encoding operation failed.
    #[error("Encoding failed: {message}")]
    EncodingFailed {
        /// Description of the encoding failure.
        message: String,
    },

    /// Sample rate conversion is not supported.
    #[error("Sample rate conversion not supported: {from_rate}Hz -> {to_rate}Hz")]
    UnsupportedSampleRateConversion {
        /// Source sample rate.
        from_rate: u32,
        /// Target sample rate.
        to_rate: u32,
    },

    /// Channel conversion is not supported.
    #[error("Channel conversion not supported: {from_channels} -> {to_channels} channels")]
    UnsupportedChannelConversion {
        /// Source channel count.
        from_channels: u8,
        /// Target channel count.
        to_channels: u8,
    },
}

/// Describes the capability to transcode between two audio formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscodeCapability {
    /// Direct transcoding is supported.
    Supported,
    /// No conversion needed (same format).
    PassThrough,
    /// Transcoding is not supported.
    Unsupported,
}

impl TranscodeCapability {
    /// Returns `true` if transcoding is possible (either supported or pass-through).
    pub const fn is_available(&self) -> bool {
        matches!(self, Self::Supported | Self::PassThrough)
    }
}

/// Opus encoder wrapper using the `audiopus` crate.
///
/// This encoder converts PCM audio (i16 samples) to Opus-encoded frames.
/// Opus is well-suited for real-time speech transmission due to its low
/// latency and good compression for voice content.
pub struct OpusEncoder {
    encoder: audiopus::coder::Encoder,
    sample_rate: u32,
    channels: u8,
    frame_size: usize,
}

impl OpusEncoder {
    /// Create a new Opus encoder.
    ///
    /// # Arguments
    ///
    /// * `sample_rate` - Sample rate in Hz (supports 8000, 12000, 16000, 24000, 48000)
    /// * `channels` - Number of channels (1 for mono, 2 for stereo)
    ///
    /// # Errors
    ///
    /// Returns `TranscodeError::EncoderInitFailed` if the encoder cannot be created.
    pub fn new(sample_rate: u32, channels: u8) -> Result<Self, TranscodeError> {
        use audiopus::{Application, Channels, SampleRate};

        let opus_sample_rate = match sample_rate {
            8000 => SampleRate::Hz8000,
            12000 => SampleRate::Hz12000,
            16000 => SampleRate::Hz16000,
            24000 => SampleRate::Hz24000,
            48000 => SampleRate::Hz48000,
            _ => {
                return Err(TranscodeError::InvalidInputFormat {
                    reason: format!("Unsupported sample rate for Opus: {sample_rate}Hz. Supported: 8000, 12000, 16000, 24000, 48000"),
                });
            }
        };

        let opus_channels = match channels {
            1 => Channels::Mono,
            2 => Channels::Stereo,
            _ => {
                return Err(TranscodeError::InvalidInputFormat {
                    reason: format!("Unsupported channel count for Opus: {channels}. Supported: 1 (mono), 2 (stereo)"),
                });
            }
        };

        let encoder =
            audiopus::coder::Encoder::new(opus_sample_rate, opus_channels, Application::Voip)
                .map_err(|e| TranscodeError::EncoderInitFailed {
                    message: format!("Failed to create Opus encoder: {e}"),
                })?;

        // Frame size for 20ms of audio
        let frame_size = (sample_rate as usize * 20) / 1000;

        debug!(sample_rate, channels, frame_size, "Created Opus encoder");

        Ok(Self {
            encoder,
            sample_rate,
            channels,
            frame_size,
        })
    }

    /// Get the sample rate this encoder was configured with.
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Get the number of channels this encoder was configured with.
    pub const fn channels(&self) -> u8 {
        self.channels
    }

    /// Get the frame size (number of samples per channel per frame).
    pub const fn frame_size(&self) -> usize {
        self.frame_size
    }

    /// Encode PCM samples to Opus.
    ///
    /// # Arguments
    ///
    /// * `pcm` - PCM samples as i16 values. The number of samples should be
    ///   a multiple of the frame size for best results.
    ///
    /// # Returns
    ///
    /// Returns a vector of encoded Opus frames.
    ///
    /// # Errors
    ///
    /// Returns `TranscodeError::EncodingFailed` if encoding fails.
    #[instrument(skip(self, pcm), fields(input_samples = pcm.len()))]
    pub fn encode(&self, pcm: &[i16]) -> Result<Vec<Vec<u8>>, TranscodeError> {
        let mut encoded_frames = Vec::new();

        // Process in frame-sized chunks
        for chunk in pcm.chunks(self.frame_size * self.channels as usize) {
            let mut frame = chunk.to_vec();

            // Pad last frame if necessary
            let expected_size = self.frame_size * self.channels as usize;
            if frame.len() < expected_size {
                frame.resize(expected_size, 0);
            }

            // Encode frame (max Opus packet size is ~4000 bytes, but speech is typically much smaller)
            let mut output = vec![0u8; 4000];
            let encoded_len = self.encoder.encode(&frame, &mut output).map_err(|e| {
                TranscodeError::EncodingFailed {
                    message: format!("Opus encoding failed: {e}"),
                }
            })?;

            output.truncate(encoded_len);
            encoded_frames.push(output);
        }

        debug!(num_frames = encoded_frames.len(), "Encoded PCM to Opus");

        Ok(encoded_frames)
    }

    /// Encode PCM samples to a single continuous Opus stream.
    ///
    /// This concatenates all encoded frames into a single buffer.
    #[instrument(skip(self, pcm), fields(input_samples = pcm.len()))]
    pub fn encode_to_bytes(&self, pcm: &[i16]) -> Result<Vec<u8>, TranscodeError> {
        let frames = self.encode(pcm)?;
        let total_size: usize = frames.iter().map(Vec::len).sum();
        let mut output = Vec::with_capacity(total_size);

        for frame in frames {
            output.extend(frame);
        }

        Ok(output)
    }
}

impl std::fmt::Debug for OpusEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpusEncoder")
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("frame_size", &self.frame_size)
            .finish_non_exhaustive()
    }
}

/// MP3 encoder wrapper using the `mp3lame-encoder` crate.
///
/// This encoder converts PCM audio to MP3 format, which is widely
/// compatible with audio players and browsers.
pub struct Mp3Encoder {
    sample_rate: u32,
    channels: u8,
    quality: Mp3Quality,
}

/// MP3 encoding quality preset.
///
/// These map to LAME's quality settings (0-9 scale, where 0 is best).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mp3Quality {
    /// Fastest encoding, lower quality (LAME quality 7).
    Fast,
    /// Balanced encoding speed and quality (LAME quality 5).
    #[default]
    Standard,
    /// High quality encoding (LAME quality 2).
    High,
    /// Best quality encoding (LAME quality 0).
    Best,
}

impl Mp3Encoder {
    /// Create a new MP3 encoder.
    ///
    /// # Arguments
    ///
    /// * `sample_rate` - Sample rate in Hz (common values: 22050, 24000, 44100, 48000)
    /// * `channels` - Number of channels (1 for mono, 2 for stereo)
    ///
    /// # Errors
    ///
    /// Returns `TranscodeError::InvalidInputFormat` if parameters are invalid.
    pub fn new(sample_rate: u32, channels: u8) -> Result<Self, TranscodeError> {
        Self::with_quality(sample_rate, channels, Mp3Quality::default())
    }

    /// Create a new MP3 encoder with a specific quality preset.
    ///
    /// # Arguments
    ///
    /// * `sample_rate` - Sample rate in Hz
    /// * `channels` - Number of channels (1 for mono, 2 for stereo)
    /// * `quality` - Encoding quality preset
    ///
    /// # Errors
    ///
    /// Returns `TranscodeError::InvalidInputFormat` if parameters are invalid.
    pub fn with_quality(
        sample_rate: u32,
        channels: u8,
        quality: Mp3Quality,
    ) -> Result<Self, TranscodeError> {
        // Validate channels
        if channels == 0 || channels > 2 {
            return Err(TranscodeError::InvalidInputFormat {
                reason: format!("Unsupported channel count for MP3: {channels}. Supported: 1 (mono), 2 (stereo)"),
            });
        }

        // Validate sample rate (LAME supports a wide range)
        if !(8000..=48000).contains(&sample_rate) {
            return Err(TranscodeError::InvalidInputFormat {
                reason: format!(
                    "Sample rate out of range for MP3: {sample_rate}Hz. Supported: 8000-48000"
                ),
            });
        }

        debug!(
            sample_rate,
            channels,
            quality = ?quality,
            "Created MP3 encoder configuration"
        );

        Ok(Self {
            sample_rate,
            channels,
            quality,
        })
    }

    /// Get the sample rate this encoder was configured with.
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Get the number of channels this encoder was configured with.
    pub const fn channels(&self) -> u8 {
        self.channels
    }

    /// Get the quality preset.
    pub const fn quality(&self) -> Mp3Quality {
        self.quality
    }

    /// Encode PCM samples to MP3.
    ///
    /// # Arguments
    ///
    /// * `pcm` - PCM samples as i16 values. For stereo, samples should be interleaved.
    ///
    /// # Returns
    ///
    /// Returns the encoded MP3 data.
    ///
    /// # Errors
    ///
    /// Returns `TranscodeError::EncoderInitFailed` or `TranscodeError::EncodingFailed`
    /// if encoding fails.
    #[instrument(skip(self, pcm), fields(input_samples = pcm.len()))]
    pub fn encode(&self, pcm: &[i16]) -> Result<Vec<u8>, TranscodeError> {
        use mp3lame_encoder::{Builder, FlushNoGap, InterleavedPcm};

        // Create encoder with the configured settings
        let mut builder = Builder::new().ok_or_else(|| TranscodeError::EncoderInitFailed {
            message: "Failed to create LAME encoder builder".to_string(),
        })?;

        builder
            .set_num_channels(self.channels)
            .map_err(|e| TranscodeError::EncoderInitFailed {
                message: format!("Failed to set channels: {e:?}"),
            })?;

        builder.set_sample_rate(self.sample_rate).map_err(|e| {
            TranscodeError::EncoderInitFailed {
                message: format!("Failed to set sample rate: {e:?}"),
            }
        })?;

        // Set quality based on preset (LAME scale: 0=best, 9=worst)
        let quality = match self.quality {
            Mp3Quality::Fast => mp3lame_encoder::Quality::Ok, // 7
            Mp3Quality::Standard => mp3lame_encoder::Quality::Good, // 5
            Mp3Quality::High => mp3lame_encoder::Quality::NearBest, // 2
            Mp3Quality::Best => mp3lame_encoder::Quality::Best, // 0
        };
        builder
            .set_quality(quality)
            .map_err(|e| TranscodeError::EncoderInitFailed {
                message: format!("Failed to set quality: {e:?}"),
            })?;

        let mut encoder = builder
            .build()
            .map_err(|e| TranscodeError::EncoderInitFailed {
                message: format!("Failed to build LAME encoder: {e:?}"),
            })?;

        // Use the library's helper to calculate required buffer size
        let output_size = mp3lame_encoder::max_required_buffer_size(pcm.len());
        let mut output: Vec<u8> = Vec::with_capacity(output_size);

        // Encode the PCM data using the library's encode_to_vec helper
        let input = InterleavedPcm(pcm);
        encoder
            .encode_to_vec(input, &mut output)
            .map_err(|e| TranscodeError::EncodingFailed {
                message: format!("MP3 encoding failed: {e:?}"),
            })?;

        // Flush the encoder to get any remaining data (needs 7200 bytes minimum)
        // The "strange error flushing buffer" message from LAME is a warning, not an error
        encoder
            .flush_to_vec::<FlushNoGap>(&mut output)
            .map_err(|e| TranscodeError::EncodingFailed {
                message: format!("MP3 flush failed: {e:?}"),
            })?;

        debug!(
            input_samples = pcm.len(),
            output_bytes = output.len(),
            "Encoded PCM to MP3"
        );

        Ok(output)
    }
}

impl std::fmt::Debug for Mp3Encoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mp3Encoder")
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("quality", &self.quality)
            .finish()
    }
}

/// Unified transcoder interface for audio format conversion.
///
/// This provides a high-level API for transcoding audio data between formats.
/// It handles the details of creating appropriate encoders and validating
/// format compatibility.
#[derive(Debug, Default)]
pub struct Transcoder;

impl Transcoder {
    /// Check if transcoding between two codecs is possible.
    ///
    /// # Arguments
    ///
    /// * `from` - Source audio codec.
    /// * `to` - Target audio codec.
    ///
    /// # Returns
    ///
    /// Returns `true` if the transcoding path is supported.
    pub const fn can_transcode(from: AudioCodec, to: AudioCodec) -> bool {
        Self::get_capability(from, to).is_available()
    }

    /// Get the transcoding capability between two codecs.
    ///
    /// # Arguments
    ///
    /// * `from` - Source audio codec.
    /// * `to` - Target audio codec.
    ///
    /// # Returns
    ///
    /// Returns the `TranscodeCapability` describing what kind of conversion is possible.
    pub const fn get_capability(from: AudioCodec, to: AudioCodec) -> TranscodeCapability {
        match (from, to) {
            // Same format is pass-through
            (AudioCodec::Pcm, AudioCodec::Pcm)
            | (AudioCodec::Opus, AudioCodec::Opus)
            | (AudioCodec::Mp3, AudioCodec::Mp3) => TranscodeCapability::PassThrough,

            // PCM to compressed formats is supported
            (AudioCodec::Pcm, AudioCodec::Opus | AudioCodec::Mp3) => TranscodeCapability::Supported,

            // Compressed to compressed or to PCM requires decoding (not supported)
            (
                AudioCodec::Opus | AudioCodec::Mp3,
                AudioCodec::Pcm | AudioCodec::Opus | AudioCodec::Mp3,
            ) => TranscodeCapability::Unsupported,
        }
    }

    /// Transcode audio data from one format to another.
    ///
    /// # Arguments
    ///
    /// * `data` - Raw audio data (for PCM input: i16 little-endian samples).
    /// * `from_format` - Description of the input audio format.
    /// * `to_format` - Description of the desired output format.
    ///
    /// # Returns
    ///
    /// Returns the transcoded audio data.
    ///
    /// # Errors
    ///
    /// Returns a `TranscodeError` if:
    /// - The transcoding path is not supported.
    /// - The input format is invalid.
    /// - Encoding fails.
    ///
    /// # Notes
    ///
    /// - For PCM input, the data must be i16 little-endian samples.
    /// - Sample rate conversion is not currently supported; the input and output
    ///   sample rates must match (or the encoder must support the input rate).
    /// - Channel conversion is not currently supported.
    #[instrument(skip(data), fields(input_bytes = data.len()))]
    pub fn transcode(
        data: &[u8],
        from_format: &AudioFormat,
        to_format: &AudioFormat,
    ) -> Result<Vec<u8>, TranscodeError> {
        let capability = Self::get_capability(from_format.codec, to_format.codec);

        match capability {
            TranscodeCapability::PassThrough => {
                debug!("Pass-through: no transcoding needed");
                Ok(data.to_vec())
            }
            TranscodeCapability::Unsupported => Err(TranscodeError::UnsupportedPath {
                from: from_format.codec,
                to: to_format.codec,
            }),
            TranscodeCapability::Supported => Self::do_transcode(data, from_format, to_format),
        }
    }

    /// Internal transcoding implementation.
    fn do_transcode(
        data: &[u8],
        from_format: &AudioFormat,
        to_format: &AudioFormat,
    ) -> Result<Vec<u8>, TranscodeError> {
        // Validate input format
        if from_format.codec != AudioCodec::Pcm {
            return Err(TranscodeError::InvalidInputFormat {
                reason: format!(
                    "Only PCM input is supported for transcoding, got: {}",
                    from_format.codec
                ),
            });
        }

        // Check bits per sample (we expect 16-bit)
        if from_format.bits_per_sample != Some(16) && from_format.bits_per_sample.is_some() {
            return Err(TranscodeError::InvalidInputFormat {
                reason: format!(
                    "Only 16-bit PCM is supported, got: {:?} bits",
                    from_format.bits_per_sample
                ),
            });
        }

        // Check channel compatibility
        if from_format.channels != to_format.channels {
            return Err(TranscodeError::UnsupportedChannelConversion {
                from_channels: from_format.channels,
                to_channels: to_format.channels,
            });
        }

        // Convert bytes to i16 samples
        let pcm_samples = bytes_to_i16_le(data)?;

        // Perform encoding based on target codec
        match to_format.codec {
            AudioCodec::Opus => {
                let encoder = OpusEncoder::new(from_format.sample_rate, from_format.channels)?;
                encoder.encode_to_bytes(&pcm_samples)
            }
            AudioCodec::Mp3 => {
                let encoder = Mp3Encoder::new(from_format.sample_rate, from_format.channels)?;
                encoder.encode(&pcm_samples)
            }
            AudioCodec::Pcm => {
                // Should not reach here due to capability check, but handle it
                Ok(data.to_vec())
            }
        }
    }
}

/// Convert a byte slice of little-endian i16 samples to a Vec<i16>.
fn bytes_to_i16_le(data: &[u8]) -> Result<Vec<i16>, TranscodeError> {
    if data.len() % 2 != 0 {
        return Err(TranscodeError::InvalidInputFormat {
            reason: format!(
                "PCM data length must be even (got {} bytes), expected i16 samples",
                data.len()
            ),
        });
    }

    let samples: Vec<i16> = data
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();

    Ok(samples)
}

/// Convert f32 samples (-1.0 to 1.0) to i16 samples.
///
/// This is a utility function for providers that generate f32 samples
/// and need to prepare them for transcoding.
pub fn f32_to_i16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|&s| {
            let clamped = s.clamp(-1.0, 1.0);
            #[allow(clippy::cast_possible_truncation)]
            let sample = (clamped * 32767.0) as i16;
            sample
        })
        .collect()
}

/// Convert i16 samples to a byte vector (little-endian).
///
/// This is a utility function for preparing PCM data for the transcoder.
pub fn i16_to_bytes_le(samples: &[i16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test TranscodeCapability
    #[test]
    fn test_transcode_capability_is_available() {
        assert!(TranscodeCapability::Supported.is_available());
        assert!(TranscodeCapability::PassThrough.is_available());
        assert!(!TranscodeCapability::Unsupported.is_available());
    }

    // Test Transcoder::can_transcode
    #[test]
    fn test_can_transcode_pcm_to_opus() {
        assert!(Transcoder::can_transcode(AudioCodec::Pcm, AudioCodec::Opus));
    }

    #[test]
    fn test_can_transcode_pcm_to_mp3() {
        assert!(Transcoder::can_transcode(AudioCodec::Pcm, AudioCodec::Mp3));
    }

    #[test]
    fn test_can_transcode_same_format() {
        assert!(Transcoder::can_transcode(AudioCodec::Pcm, AudioCodec::Pcm));
        assert!(Transcoder::can_transcode(
            AudioCodec::Opus,
            AudioCodec::Opus
        ));
        assert!(Transcoder::can_transcode(AudioCodec::Mp3, AudioCodec::Mp3));
    }

    #[test]
    fn test_cannot_transcode_opus_to_pcm() {
        assert!(!Transcoder::can_transcode(
            AudioCodec::Opus,
            AudioCodec::Pcm
        ));
    }

    #[test]
    fn test_cannot_transcode_opus_to_mp3() {
        assert!(!Transcoder::can_transcode(
            AudioCodec::Opus,
            AudioCodec::Mp3
        ));
    }

    #[test]
    fn test_cannot_transcode_mp3_to_opus() {
        assert!(!Transcoder::can_transcode(
            AudioCodec::Mp3,
            AudioCodec::Opus
        ));
    }

    #[test]
    fn test_cannot_transcode_mp3_to_pcm() {
        assert!(!Transcoder::can_transcode(AudioCodec::Mp3, AudioCodec::Pcm));
    }

    // Test get_capability
    #[test]
    fn test_get_capability() {
        assert_eq!(
            Transcoder::get_capability(AudioCodec::Pcm, AudioCodec::Pcm),
            TranscodeCapability::PassThrough
        );
        assert_eq!(
            Transcoder::get_capability(AudioCodec::Pcm, AudioCodec::Opus),
            TranscodeCapability::Supported
        );
        assert_eq!(
            Transcoder::get_capability(AudioCodec::Opus, AudioCodec::Mp3),
            TranscodeCapability::Unsupported
        );
    }

    // Test bytes_to_i16_le
    #[test]
    fn test_bytes_to_i16_le() {
        let bytes = [0x00, 0x80]; // -32768 in little-endian
        let samples = bytes_to_i16_le(&bytes).unwrap();
        assert_eq!(samples, vec![-32768i16]);

        let bytes = [0xFF, 0x7F]; // 32767 in little-endian
        let samples = bytes_to_i16_le(&bytes).unwrap();
        assert_eq!(samples, vec![32767i16]);

        let bytes = [0x00, 0x00]; // 0
        let samples = bytes_to_i16_le(&bytes).unwrap();
        assert_eq!(samples, vec![0i16]);
    }

    #[test]
    fn test_bytes_to_i16_le_odd_length() {
        let bytes = [0x00, 0x80, 0xFF]; // Odd length
        let result = bytes_to_i16_le(&bytes);
        assert!(matches!(
            result,
            Err(TranscodeError::InvalidInputFormat { .. })
        ));
    }

    // Test f32_to_i16
    #[test]
    fn test_f32_to_i16() {
        let samples = f32_to_i16(&[0.0, 1.0, -1.0, 0.5, -0.5]);
        assert_eq!(samples[0], 0);
        assert_eq!(samples[1], 32767);
        assert_eq!(samples[2], -32767);
        assert_eq!(samples[3], 16383); // 0.5 * 32767 ≈ 16383
        assert_eq!(samples[4], -16383);
    }

    #[test]
    fn test_f32_to_i16_clamping() {
        // Values outside [-1.0, 1.0] should be clamped
        let samples = f32_to_i16(&[2.0, -2.0]);
        assert_eq!(samples[0], 32767);
        assert_eq!(samples[1], -32767);
    }

    // Test i16_to_bytes_le
    #[test]
    fn test_i16_to_bytes_le() {
        let bytes = i16_to_bytes_le(&[-32768i16, 32767i16, 0i16]);
        assert_eq!(bytes, vec![0x00, 0x80, 0xFF, 0x7F, 0x00, 0x00]);
    }

    // Test OpusEncoder
    #[test]
    fn test_opus_encoder_creation() {
        let encoder = OpusEncoder::new(24000, 1);
        assert!(encoder.is_ok());

        let encoder = encoder.unwrap();
        assert_eq!(encoder.sample_rate(), 24000);
        assert_eq!(encoder.channels(), 1);
        assert_eq!(encoder.frame_size(), 480); // 20ms at 24kHz
    }

    #[test]
    fn test_opus_encoder_invalid_sample_rate() {
        let result = OpusEncoder::new(22050, 1);
        assert!(matches!(
            result,
            Err(TranscodeError::InvalidInputFormat { .. })
        ));
    }

    #[test]
    fn test_opus_encoder_invalid_channels() {
        let result = OpusEncoder::new(24000, 3);
        assert!(matches!(
            result,
            Err(TranscodeError::InvalidInputFormat { .. })
        ));
    }

    #[test]
    fn test_opus_encoder_encode() {
        let encoder = OpusEncoder::new(24000, 1).unwrap();

        // Generate 20ms of silence (480 samples at 24kHz)
        let pcm: Vec<i16> = vec![0; 480];

        let result = encoder.encode(&pcm);
        assert!(result.is_ok());

        let frames = result.unwrap();
        assert_eq!(frames.len(), 1);
        assert!(!frames[0].is_empty());
    }

    #[test]
    fn test_opus_encoder_encode_to_bytes() {
        let encoder = OpusEncoder::new(24000, 1).unwrap();
        let pcm: Vec<i16> = vec![0; 480];

        let result = encoder.encode_to_bytes(&pcm);
        assert!(result.is_ok());
        assert!(!result.unwrap().is_empty());
    }

    // Test Mp3Encoder
    #[test]
    fn test_mp3_encoder_creation() {
        let encoder = Mp3Encoder::new(24000, 1);
        assert!(encoder.is_ok());

        let encoder = encoder.unwrap();
        assert_eq!(encoder.sample_rate(), 24000);
        assert_eq!(encoder.channels(), 1);
        assert_eq!(encoder.quality(), Mp3Quality::Standard);
    }

    #[test]
    fn test_mp3_encoder_with_quality() {
        let encoder = Mp3Encoder::with_quality(44100, 2, Mp3Quality::High);
        assert!(encoder.is_ok());

        let encoder = encoder.unwrap();
        assert_eq!(encoder.sample_rate(), 44100);
        assert_eq!(encoder.channels(), 2);
        assert_eq!(encoder.quality(), Mp3Quality::High);
    }

    #[test]
    fn test_mp3_encoder_invalid_channels() {
        let result = Mp3Encoder::new(24000, 0);
        assert!(matches!(
            result,
            Err(TranscodeError::InvalidInputFormat { .. })
        ));

        let result = Mp3Encoder::new(24000, 3);
        assert!(matches!(
            result,
            Err(TranscodeError::InvalidInputFormat { .. })
        ));
    }

    #[test]
    fn test_mp3_encoder_encode() {
        let encoder = Mp3Encoder::new(24000, 1).unwrap();

        // Generate enough test audio to produce output.
        // MP3 frame size is 1152 samples, but LAME may need multiple frames
        // to produce output due to its encoding algorithm.
        // Using 10 frames worth of samples to ensure we get valid MP3 output.
        let pcm: Vec<i16> = vec![0; 1152 * 10];

        let result = encoder.encode(&pcm);
        assert!(result.is_ok(), "Encoding should succeed");

        let encoded = result.unwrap();
        // MP3 output should not be empty when given sufficient input
        // Note: For very small inputs, LAME may produce empty output during encode
        // and only produce data during flush
        assert!(
            !encoded.is_empty(),
            "MP3 output should not be empty with {} samples",
            pcm.len()
        );
    }

    // Test Transcoder::transcode
    #[test]
    fn test_transcode_pass_through() {
        let pcm_format = AudioFormat {
            codec: AudioCodec::Pcm,
            sample_rate: 24000,
            channels: 1,
            bits_per_sample: Some(16),
        };

        let data = vec![0u8; 100];
        let result = Transcoder::transcode(&data, &pcm_format, &pcm_format);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), data);
    }

    #[test]
    fn test_transcode_unsupported_path() {
        let opus_format = AudioFormat {
            codec: AudioCodec::Opus,
            sample_rate: 24000,
            channels: 1,
            bits_per_sample: None,
        };
        let mp3_format = AudioFormat {
            codec: AudioCodec::Mp3,
            sample_rate: 24000,
            channels: 1,
            bits_per_sample: None,
        };

        let data = vec![0u8; 100];
        let result = Transcoder::transcode(&data, &opus_format, &mp3_format);
        assert!(matches!(
            result,
            Err(TranscodeError::UnsupportedPath { .. })
        ));
    }

    #[test]
    fn test_transcode_pcm_to_opus() {
        let pcm_format = AudioFormat {
            codec: AudioCodec::Pcm,
            sample_rate: 24000,
            channels: 1,
            bits_per_sample: Some(16),
        };
        let opus_format = AudioFormat {
            codec: AudioCodec::Opus,
            sample_rate: 24000,
            channels: 1,
            bits_per_sample: None,
        };

        // Generate 20ms of PCM data (480 samples * 2 bytes per sample)
        let pcm_bytes = i16_to_bytes_le(&vec![0i16; 480]);

        let result = Transcoder::transcode(&pcm_bytes, &pcm_format, &opus_format);
        assert!(result.is_ok());

        let encoded = result.unwrap();
        assert!(!encoded.is_empty());
        // Opus data should be much smaller than PCM
        assert!(encoded.len() < pcm_bytes.len());
    }

    #[test]
    fn test_transcode_pcm_to_mp3() {
        let pcm_format = AudioFormat {
            codec: AudioCodec::Pcm,
            sample_rate: 24000,
            channels: 1,
            bits_per_sample: Some(16),
        };
        let mp3_format = AudioFormat {
            codec: AudioCodec::Mp3,
            sample_rate: 24000,
            channels: 1,
            bits_per_sample: None,
        };

        // Generate enough PCM data for reliable MP3 output.
        // Using 10 MP3 frames worth of samples (1152 * 10 samples).
        let pcm_bytes = i16_to_bytes_le(&vec![0i16; 1152 * 10]);

        let result = Transcoder::transcode(&pcm_bytes, &pcm_format, &mp3_format);
        assert!(result.is_ok(), "Transcoding should succeed");

        let encoded = result.unwrap();
        assert!(
            !encoded.is_empty(),
            "Transcoded MP3 output should not be empty"
        );
    }

    #[test]
    fn test_transcode_channel_mismatch() {
        let mono_format = AudioFormat {
            codec: AudioCodec::Pcm,
            sample_rate: 24000,
            channels: 1,
            bits_per_sample: Some(16),
        };
        let stereo_format = AudioFormat {
            codec: AudioCodec::Opus,
            sample_rate: 24000,
            channels: 2,
            bits_per_sample: None,
        };

        let data = vec![0u8; 100];
        let result = Transcoder::transcode(&data, &mono_format, &stereo_format);
        assert!(matches!(
            result,
            Err(TranscodeError::UnsupportedChannelConversion { .. })
        ));
    }

    #[test]
    fn test_transcode_invalid_pcm_length() {
        let pcm_format = AudioFormat {
            codec: AudioCodec::Pcm,
            sample_rate: 24000,
            channels: 1,
            bits_per_sample: Some(16),
        };
        let opus_format = AudioFormat {
            codec: AudioCodec::Opus,
            sample_rate: 24000,
            channels: 1,
            bits_per_sample: None,
        };

        // Odd number of bytes is invalid for i16 samples
        let data = vec![0u8; 101];
        let result = Transcoder::transcode(&data, &pcm_format, &opus_format);
        assert!(matches!(
            result,
            Err(TranscodeError::InvalidInputFormat { .. })
        ));
    }

    // Test Mp3Quality default
    #[test]
    fn test_mp3_quality_default() {
        assert_eq!(Mp3Quality::default(), Mp3Quality::Standard);
    }

    // Test Debug implementations
    #[test]
    fn test_opus_encoder_debug() {
        let encoder = OpusEncoder::new(24000, 1).unwrap();
        let debug_str = format!("{encoder:?}");
        assert!(debug_str.contains("OpusEncoder"));
        assert!(debug_str.contains("24000"));
    }

    #[test]
    fn test_mp3_encoder_debug() {
        let encoder = Mp3Encoder::new(24000, 1).unwrap();
        let debug_str = format!("{encoder:?}");
        assert!(debug_str.contains("Mp3Encoder"));
        assert!(debug_str.contains("24000"));
    }

    // Test roundtrip conversion utilities
    #[test]
    fn test_f32_i16_bytes_roundtrip() {
        let original_f32 = vec![0.0f32, 0.5, -0.5, 1.0, -1.0];
        let i16_samples = f32_to_i16(&original_f32);
        let bytes = i16_to_bytes_le(&i16_samples);
        let recovered_i16 = bytes_to_i16_le(&bytes).unwrap();
        assert_eq!(i16_samples, recovered_i16);
    }
}
