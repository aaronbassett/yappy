//! Voice configuration integration tests for the Yappy TTS server
//!
//! These tests verify voice selection and configuration behavior:
//! - Voice validation in session.init
//! - invalid_voice error with alternatives
//! - Voice parameter validation (speed, pitch, volume)

use yappy_core::{ClientMessage, ServerMessage, VoiceConfig};

use crate::common::{
    connect_ws, receive_server_message_with_timeout, send_message, spawn_test_server,
    TestServerBuilder, DEFAULT_TIMEOUT,
};

// ============================================================================
// Voice Selection Tests
// ============================================================================

/// Test: Session initialization with a valid voice ID succeeds
#[tokio::test]
async fn test_session_with_valid_voice() {
    let server = spawn_test_server().await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Request a valid voice (mock provider has "test_voice_1" and "test_voice_2")
    let init_msg = ClientMessage::SessionInit {
        provider: None,
        voice: Some(VoiceConfig {
            id: "test_voice_1".to_string(),
            speed: 1.0,
            pitch: 0.0,
            volume: 1.0,
        }),
        audio_format: None,
        code_block_mode: None,
    };
    send_message(&mut ws, &init_msg)
        .await
        .expect("Should send init message");

    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::SessionReady {
            session_id, voice, ..
        } => {
            assert!(session_id.starts_with("ses_"));
            assert_eq!(voice, "test_voice_1", "Should use requested voice");
        }
        other => {
            panic!("Expected SessionReady, got: {other:?}");
        }
    }

    server.shutdown();
}

/// Test: Session initialization with an invalid voice ID returns error with alternatives
#[tokio::test]
async fn test_session_with_invalid_voice_returns_alternatives() {
    let server = spawn_test_server().await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Request an invalid voice
    let init_msg = ClientMessage::SessionInit {
        provider: None,
        voice: Some(VoiceConfig {
            id: "nonexistent_voice".to_string(),
            speed: 1.0,
            pitch: 0.0,
            volume: 1.0,
        }),
        audio_format: None,
        code_block_mode: None,
    };
    send_message(&mut ws, &init_msg)
        .await
        .expect("Should send init message");

    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::SessionError {
            code,
            message,
            alternatives,
        } => {
            assert_eq!(code, "invalid_voice", "Should have invalid_voice error code");
            assert!(
                message.contains("nonexistent_voice"),
                "Message should reference the requested voice: {message}"
            );

            // Should include alternatives (available voices)
            let alts = alternatives.expect("Should have alternatives array");
            assert!(
                alts.contains(&"test_voice_1".to_string()),
                "Alternatives should include available voices: {alts:?}"
            );
        }
        ServerMessage::SessionReady { .. } => {
            panic!("Should reject invalid voice, not silently accept");
        }
        other => {
            panic!("Expected SessionError, got: {other:?}");
        }
    }

    server.shutdown();
}

/// Test: Default voice is used when none specified
#[tokio::test]
async fn test_session_uses_default_voice_when_none_specified() {
    let server = spawn_test_server().await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Don't specify a voice
    let init_msg = ClientMessage::SessionInit {
        provider: None,
        voice: None,
        audio_format: None,
        code_block_mode: None,
    };
    send_message(&mut ws, &init_msg)
        .await
        .expect("Should send init message");

    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::SessionReady { voice, .. } => {
            // Should use the provider's first/default voice
            assert!(!voice.is_empty(), "Should have a voice assigned");
        }
        other => {
            panic!("Expected SessionReady, got: {other:?}");
        }
    }

    server.shutdown();
}

// ============================================================================
// Voice Parameter Validation Tests
// ============================================================================

/// Test: Voice parameters at boundary values are accepted
#[tokio::test]
async fn test_voice_parameters_at_boundaries() {
    let server = spawn_test_server().await;

    // Test minimum valid speed (0.5)
    {
        let mut ws = connect_ws(&server)
            .await
            .expect("Should connect to WebSocket");

        let init_msg = ClientMessage::SessionInit {
            provider: None,
            voice: Some(VoiceConfig {
                id: "test_voice_1".to_string(),
                speed: 0.5, // Minimum valid speed
                pitch: -1.0, // Minimum valid pitch
                volume: 0.0, // Minimum valid volume
            }),
            audio_format: None,
            code_block_mode: None,
        };
        send_message(&mut ws, &init_msg)
            .await
            .expect("Should send init message");

        let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
            .await
            .expect("Should receive response");

        assert!(
            matches!(response, ServerMessage::SessionReady { .. }),
            "Should accept minimum boundary values: {response:?}"
        );
    }

    // Test maximum valid speed (2.0)
    {
        let mut ws = connect_ws(&server)
            .await
            .expect("Should connect to WebSocket");

        let init_msg = ClientMessage::SessionInit {
            provider: None,
            voice: Some(VoiceConfig {
                id: "test_voice_1".to_string(),
                speed: 2.0, // Maximum valid speed
                pitch: 1.0, // Maximum valid pitch
                volume: 1.0, // Maximum valid volume
            }),
            audio_format: None,
            code_block_mode: None,
        };
        send_message(&mut ws, &init_msg)
            .await
            .expect("Should send init message");

        let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
            .await
            .expect("Should receive response");

        assert!(
            matches!(response, ServerMessage::SessionReady { .. }),
            "Should accept maximum boundary values: {response:?}"
        );
    }

    server.shutdown();
}

/// Test: Invalid speed (too low) returns error
#[tokio::test]
async fn test_invalid_speed_too_low() {
    let server = spawn_test_server().await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    let init_msg = ClientMessage::SessionInit {
        provider: None,
        voice: Some(VoiceConfig {
            id: "test_voice_1".to_string(),
            speed: 0.4, // Below minimum (0.5)
            pitch: 0.0,
            volume: 1.0,
        }),
        audio_format: None,
        code_block_mode: None,
    };
    send_message(&mut ws, &init_msg)
        .await
        .expect("Should send init message");

    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::SessionError { code, message, .. } => {
            assert_eq!(
                code, "invalid_voice_config",
                "Should have invalid_voice_config error code"
            );
            assert!(
                message.contains("speed"),
                "Message should mention speed: {message}"
            );
        }
        ServerMessage::SessionReady { .. } => {
            panic!("Should reject invalid speed value");
        }
        other => {
            panic!("Expected SessionError, got: {other:?}");
        }
    }

    server.shutdown();
}

/// Test: Invalid speed (too high) returns error
#[tokio::test]
async fn test_invalid_speed_too_high() {
    let server = spawn_test_server().await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    let init_msg = ClientMessage::SessionInit {
        provider: None,
        voice: Some(VoiceConfig {
            id: "test_voice_1".to_string(),
            speed: 2.5, // Above maximum (2.0)
            pitch: 0.0,
            volume: 1.0,
        }),
        audio_format: None,
        code_block_mode: None,
    };
    send_message(&mut ws, &init_msg)
        .await
        .expect("Should send init message");

    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::SessionError { code, message, .. } => {
            assert_eq!(code, "invalid_voice_config");
            assert!(message.contains("speed"), "Message should mention speed");
        }
        other => {
            panic!("Expected SessionError, got: {other:?}");
        }
    }

    server.shutdown();
}

/// Test: Invalid pitch returns error
#[tokio::test]
async fn test_invalid_pitch_out_of_range() {
    let server = spawn_test_server().await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    let init_msg = ClientMessage::SessionInit {
        provider: None,
        voice: Some(VoiceConfig {
            id: "test_voice_1".to_string(),
            speed: 1.0,
            pitch: 1.5, // Above maximum (1.0)
            volume: 1.0,
        }),
        audio_format: None,
        code_block_mode: None,
    };
    send_message(&mut ws, &init_msg)
        .await
        .expect("Should send init message");

    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::SessionError { code, message, .. } => {
            assert_eq!(code, "invalid_voice_config");
            assert!(message.contains("pitch"), "Message should mention pitch");
        }
        other => {
            panic!("Expected SessionError, got: {other:?}");
        }
    }

    server.shutdown();
}

/// Test: Invalid volume returns error
#[tokio::test]
async fn test_invalid_volume_out_of_range() {
    let server = spawn_test_server().await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    let init_msg = ClientMessage::SessionInit {
        provider: None,
        voice: Some(VoiceConfig {
            id: "test_voice_1".to_string(),
            speed: 1.0,
            pitch: 0.0,
            volume: 1.5, // Above maximum (1.0)
        }),
        audio_format: None,
        code_block_mode: None,
    };
    send_message(&mut ws, &init_msg)
        .await
        .expect("Should send init message");

    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::SessionError { code, message, .. } => {
            assert_eq!(code, "invalid_voice_config");
            assert!(message.contains("volume"), "Message should mention volume");
        }
        other => {
            panic!("Expected SessionError, got: {other:?}");
        }
    }

    server.shutdown();
}

/// Test: Negative volume returns error
#[tokio::test]
async fn test_invalid_volume_negative() {
    let server = spawn_test_server().await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    let init_msg = ClientMessage::SessionInit {
        provider: None,
        voice: Some(VoiceConfig {
            id: "test_voice_1".to_string(),
            speed: 1.0,
            pitch: 0.0,
            volume: -0.5, // Below minimum (0.0)
        }),
        audio_format: None,
        code_block_mode: None,
    };
    send_message(&mut ws, &init_msg)
        .await
        .expect("Should send init message");

    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::SessionError { code, message, .. } => {
            assert_eq!(code, "invalid_voice_config");
            assert!(message.contains("volume"), "Message should mention volume");
        }
        other => {
            panic!("Expected SessionError, got: {other:?}");
        }
    }

    server.shutdown();
}

// ============================================================================
// Provider-Specific Voice Tests
// ============================================================================

/// Test: Voice validation uses the correct provider's voice list
#[tokio::test]
async fn test_voice_validation_uses_provider_voice_list() {
    // Create server with two providers that have different voices
    let server = TestServerBuilder::new()
        .with_mock_provider_and_voice("provider_a", "voice_a")
        .with_mock_provider_and_voice("provider_b", "voice_b")
        .with_default_provider("provider_a")
        .spawn()
        .await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Request provider_a with voice_b (which belongs to provider_b)
    let init_msg = ClientMessage::SessionInit {
        provider: Some("provider_a".to_string()),
        voice: Some(VoiceConfig {
            id: "voice_b".to_string(), // Wrong voice for this provider
            speed: 1.0,
            pitch: 0.0,
            volume: 1.0,
        }),
        audio_format: None,
        code_block_mode: None,
    };
    send_message(&mut ws, &init_msg)
        .await
        .expect("Should send init message");

    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::SessionError {
            code, alternatives, ..
        } => {
            assert_eq!(code, "invalid_voice");
            // Alternatives should be from provider_a, not provider_b
            let alts = alternatives.expect("Should have alternatives");
            assert!(
                alts.contains(&"voice_a".to_string()),
                "Should suggest provider_a's voice"
            );
            assert!(
                !alts.contains(&"voice_b".to_string()),
                "Should NOT suggest provider_b's voice"
            );
        }
        other => {
            panic!("Expected SessionError, got: {other:?}");
        }
    }

    server.shutdown();
}
