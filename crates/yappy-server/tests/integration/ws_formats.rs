//! Audio format negotiation integration tests
//!
//! Tests the audio format negotiation behavior in session.init,
//! including native format support and transcoding capabilities.

use crate::common::{
    receive_server_message_with_timeout, send_message, MockTtsProvider, TestServerBuilder,
    DEFAULT_TIMEOUT,
};
use yappy_core::{
    audio::{AudioCodec, AudioFormat},
    ClientMessage, ServerMessage,
};

// ============================================================================
// Native Format Support Tests
// ============================================================================

/// Test that session.init succeeds when requesting a natively supported format
#[tokio::test]
async fn test_session_init_with_native_opus_format() {
    let server = TestServerBuilder::new()
        .with_provider(MockTtsProvider::with_formats(
            "format_provider",
            vec![
                AudioFormat {
                    codec: AudioCodec::Opus,
                    sample_rate: 48000,
                    channels: 1,
                    bits_per_sample: None,
                },
                AudioFormat {
                    codec: AudioCodec::Pcm,
                    sample_rate: 24000,
                    channels: 1,
                    bits_per_sample: Some(16),
                },
            ],
        ))
        .with_default_provider("format_provider")
        .spawn()
        .await;

    let mut ws = server.connect_ws().await;

    // Request Opus format (natively supported)
    let init = ClientMessage::SessionInit {
        provider: Some("format_provider".to_string()),
        voice: None,
        audio_format: Some(AudioFormat {
            codec: AudioCodec::Opus,
            sample_rate: 48000,
            channels: 1,
            bits_per_sample: None,
        }),
        code_block_mode: None,
    };
    send_message(&mut ws, &init).await.unwrap();

    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .unwrap();

    match response {
        ServerMessage::SessionReady { audio_format, .. } => {
            assert_eq!(
                audio_format.codec,
                AudioCodec::Opus,
                "Should confirm Opus format"
            );
            assert_eq!(audio_format.sample_rate, 48000);
            assert_eq!(audio_format.channels, 1);
        }
        other => panic!("Expected SessionReady, got: {other:?}"),
    }
}

/// Test that session.init succeeds when requesting native PCM format
#[tokio::test]
async fn test_session_init_with_native_pcm_format() {
    let server = TestServerBuilder::new()
        .with_provider(MockTtsProvider::with_formats(
            "format_provider",
            vec![
                AudioFormat {
                    codec: AudioCodec::Opus,
                    sample_rate: 48000,
                    channels: 1,
                    bits_per_sample: None,
                },
                AudioFormat {
                    codec: AudioCodec::Pcm,
                    sample_rate: 24000,
                    channels: 1,
                    bits_per_sample: Some(16),
                },
            ],
        ))
        .with_default_provider("format_provider")
        .spawn()
        .await;

    let mut ws = server.connect_ws().await;

    // Request PCM format (natively supported)
    let init = ClientMessage::SessionInit {
        provider: Some("format_provider".to_string()),
        voice: None,
        audio_format: Some(AudioFormat {
            codec: AudioCodec::Pcm,
            sample_rate: 24000,
            channels: 1,
            bits_per_sample: Some(16),
        }),
        code_block_mode: None,
    };
    send_message(&mut ws, &init).await.unwrap();

    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .unwrap();

    match response {
        ServerMessage::SessionReady { audio_format, .. } => {
            assert_eq!(
                audio_format.codec,
                AudioCodec::Pcm,
                "Should confirm PCM format"
            );
            assert_eq!(audio_format.sample_rate, 24000);
        }
        other => panic!("Expected SessionReady, got: {other:?}"),
    }
}

// ============================================================================
// Transcoding Format Tests
// ============================================================================

/// Test that session.init succeeds when transcoding from PCM to Opus is needed
#[tokio::test]
async fn test_session_init_with_transcode_pcm_to_opus() {
    // Provider only supports PCM, but client requests Opus
    let server = TestServerBuilder::new()
        .with_provider(MockTtsProvider::with_formats(
            "pcm_provider",
            vec![AudioFormat {
                codec: AudioCodec::Pcm,
                sample_rate: 24000,
                channels: 1,
                bits_per_sample: Some(16),
            }],
        ))
        .with_default_provider("pcm_provider")
        .spawn()
        .await;

    let mut ws = server.connect_ws().await;

    // Request Opus format (requires transcoding from PCM)
    let init = ClientMessage::SessionInit {
        provider: Some("pcm_provider".to_string()),
        voice: None,
        audio_format: Some(AudioFormat {
            codec: AudioCodec::Opus,
            sample_rate: 24000,
            channels: 1,
            bits_per_sample: None,
        }),
        code_block_mode: None,
    };
    send_message(&mut ws, &init).await.unwrap();

    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .unwrap();

    match response {
        ServerMessage::SessionReady { audio_format, .. } => {
            assert_eq!(
                audio_format.codec,
                AudioCodec::Opus,
                "Should confirm Opus format (via transcoding)"
            );
        }
        other => panic!("Expected SessionReady, got: {other:?}"),
    }
}

/// Test that session.init succeeds when transcoding from PCM to MP3 is needed
#[tokio::test]
async fn test_session_init_with_transcode_pcm_to_mp3() {
    // Provider only supports PCM, but client requests MP3
    let server = TestServerBuilder::new()
        .with_provider(MockTtsProvider::with_formats(
            "pcm_provider",
            vec![AudioFormat {
                codec: AudioCodec::Pcm,
                sample_rate: 24000,
                channels: 1,
                bits_per_sample: Some(16),
            }],
        ))
        .with_default_provider("pcm_provider")
        .spawn()
        .await;

    let mut ws = server.connect_ws().await;

    // Request MP3 format (requires transcoding from PCM)
    let init = ClientMessage::SessionInit {
        provider: Some("pcm_provider".to_string()),
        voice: None,
        audio_format: Some(AudioFormat {
            codec: AudioCodec::Mp3,
            sample_rate: 24000,
            channels: 1,
            bits_per_sample: None,
        }),
        code_block_mode: None,
    };
    send_message(&mut ws, &init).await.unwrap();

    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .unwrap();

    match response {
        ServerMessage::SessionReady { audio_format, .. } => {
            assert_eq!(
                audio_format.codec,
                AudioCodec::Mp3,
                "Should confirm MP3 format (via transcoding)"
            );
        }
        other => panic!("Expected SessionReady, got: {other:?}"),
    }
}

// ============================================================================
// Invalid Format Tests
// ============================================================================

/// Test that session.init returns `invalid_format` when format cannot be provided
#[tokio::test]
async fn test_session_init_error_invalid_format() {
    // Provider only supports Opus (no PCM, so can't transcode to MP3)
    let server = TestServerBuilder::new()
        .with_provider(MockTtsProvider::with_formats(
            "opus_only_provider",
            vec![AudioFormat {
                codec: AudioCodec::Opus,
                sample_rate: 48000,
                channels: 1,
                bits_per_sample: None,
            }],
        ))
        .with_default_provider("opus_only_provider")
        .spawn()
        .await;

    let mut ws = server.connect_ws().await;

    // Request MP3 format (not natively supported, and can't transcode from Opus)
    let init = ClientMessage::SessionInit {
        provider: Some("opus_only_provider".to_string()),
        voice: None,
        audio_format: Some(AudioFormat {
            codec: AudioCodec::Mp3,
            sample_rate: 48000,
            channels: 1,
            bits_per_sample: None,
        }),
        code_block_mode: None,
    };
    send_message(&mut ws, &init).await.unwrap();

    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .unwrap();

    match response {
        ServerMessage::SessionError { code, message, .. } => {
            assert_eq!(code, "invalid_format", "Should return invalid_format error");
            assert!(
                message.contains("mp3") || message.contains("MP3"),
                "Error message should mention the requested format: {message}"
            );
            assert!(
                message.contains("not available"),
                "Error message should explain format is not available: {message}"
            );
        }
        other => panic!("Expected SessionError with invalid_format, got: {other:?}"),
    }
}

/// Test that PCM to PCM is not a problem (no transcoding needed)
#[tokio::test]
async fn test_session_init_error_unsupported_transcode_path() {
    // Provider only supports MP3 (hypothetically), can't transcode to Opus
    // This tests the case where neither native support nor transcoding is available
    let server = TestServerBuilder::new()
        .with_provider(MockTtsProvider::with_formats(
            "mp3_only_provider",
            vec![AudioFormat {
                codec: AudioCodec::Mp3,
                sample_rate: 44100,
                channels: 2,
                bits_per_sample: None,
            }],
        ))
        .with_default_provider("mp3_only_provider")
        .spawn()
        .await;

    let mut ws = server.connect_ws().await;

    // Request Opus format (not supported, can't transcode from MP3)
    let init = ClientMessage::SessionInit {
        provider: Some("mp3_only_provider".to_string()),
        voice: None,
        audio_format: Some(AudioFormat {
            codec: AudioCodec::Opus,
            sample_rate: 48000,
            channels: 1,
            bits_per_sample: None,
        }),
        code_block_mode: None,
    };
    send_message(&mut ws, &init).await.unwrap();

    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .unwrap();

    match response {
        ServerMessage::SessionError { code, .. } => {
            assert_eq!(code, "invalid_format", "Should return invalid_format error");
        }
        other => panic!("Expected SessionError with invalid_format, got: {other:?}"),
    }
}

// ============================================================================
// Default Format Tests
// ============================================================================

/// Test that default format is used when no format is specified
#[tokio::test]
async fn test_session_init_uses_default_format() {
    let server = TestServerBuilder::new()
        .with_provider(MockTtsProvider::with_formats(
            "multi_format_provider",
            vec![
                // First format is the default
                AudioFormat {
                    codec: AudioCodec::Pcm,
                    sample_rate: 24000,
                    channels: 1,
                    bits_per_sample: Some(16),
                },
                AudioFormat {
                    codec: AudioCodec::Opus,
                    sample_rate: 48000,
                    channels: 1,
                    bits_per_sample: None,
                },
            ],
        ))
        .with_default_provider("multi_format_provider")
        .spawn()
        .await;

    let mut ws = server.connect_ws().await;

    // No audio_format specified - should use provider's default (first one)
    let init = ClientMessage::SessionInit {
        provider: Some("multi_format_provider".to_string()),
        voice: None,
        audio_format: None,
        code_block_mode: None,
    };
    send_message(&mut ws, &init).await.unwrap();

    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .unwrap();

    match response {
        ServerMessage::SessionReady { audio_format, .. } => {
            // Should use the first format (PCM) as default
            assert_eq!(
                audio_format.codec,
                AudioCodec::Pcm,
                "Should use first supported format as default"
            );
        }
        other => panic!("Expected SessionReady, got: {other:?}"),
    }
}

/// Test session.ready confirms the negotiated format
#[tokio::test]
async fn test_session_ready_includes_negotiated_format() {
    let server = TestServerBuilder::new()
        .with_provider(MockTtsProvider::with_formats(
            "format_provider",
            vec![AudioFormat {
                codec: AudioCodec::Opus,
                sample_rate: 48000,
                channels: 1,
                bits_per_sample: None,
            }],
        ))
        .with_default_provider("format_provider")
        .spawn()
        .await;

    let mut ws = server.connect_ws().await;

    let init = ClientMessage::SessionInit {
        provider: Some("format_provider".to_string()),
        voice: None,
        audio_format: Some(AudioFormat {
            codec: AudioCodec::Opus,
            sample_rate: 48000,
            channels: 1,
            bits_per_sample: None,
        }),
        code_block_mode: None,
    };
    send_message(&mut ws, &init).await.unwrap();

    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .unwrap();

    match response {
        ServerMessage::SessionReady {
            session_id,
            audio_format,
            voice,
        } => {
            // Verify all fields are present
            assert!(!session_id.is_empty(), "Session ID should be present");
            assert_eq!(audio_format.codec, AudioCodec::Opus);
            assert_eq!(audio_format.sample_rate, 48000);
            assert_eq!(audio_format.channels, 1);
            assert!(!voice.is_empty(), "Voice should be confirmed");
        }
        other => panic!("Expected SessionReady, got: {other:?}"),
    }
}
