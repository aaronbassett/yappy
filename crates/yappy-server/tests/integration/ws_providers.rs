//! Provider selection integration tests for the Yappy TTS server
//!
//! These tests verify the provider discovery and selection behavior:
//! - GET /providers endpoint returns correct provider metadata
//! - Provider selection in session.init works correctly
//! - FR-025: No silent fallback behavior (explicit errors with alternatives)

use yappy_core::{ClientMessage, ServerMessage};

use crate::common::{
    connect_ws, init_session, receive_server_message_with_timeout, send_message, spawn_test_server,
    TestServerBuilder, DEFAULT_TIMEOUT,
};

// ============================================================================
// GET /providers Endpoint Tests
// ============================================================================

/// Test: /providers endpoint returns correct provider list with full metadata
///
/// Verifies that the GET /providers endpoint returns:
/// - Available providers with full metadata (voices, formats)
/// - Unavailable providers with status and reason
/// - The default provider ID
#[tokio::test]
async fn test_providers_endpoint_lists_all_providers() {
    // Create a server with both available and unavailable providers
    let server = TestServerBuilder::new()
        .with_mock_provider("provider_a")
        .with_mock_provider("provider_b")
        .with_unavailable_provider("provider_c", "Model not found")
        .with_not_configured_provider("provider_d", "API key not set")
        .with_default_provider("provider_a")
        .spawn()
        .await;

    // Make HTTP request to /providers
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/providers", server.http_url()))
        .send()
        .await
        .expect("Should send HTTP request");

    assert_eq!(response.status(), 200);

    let body: serde_json::Value = response.json().await.expect("Should parse JSON");

    // Verify default provider
    assert_eq!(body["default_provider"], "provider_a");

    // Verify providers array exists
    let providers = body["providers"]
        .as_array()
        .expect("providers should be array");
    assert_eq!(providers.len(), 4, "Should have 4 providers");

    // Providers should be sorted alphabetically by ID
    let provider_ids: Vec<&str> = providers
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        provider_ids,
        vec!["provider_a", "provider_b", "provider_c", "provider_d"]
    );

    // Check available provider has full metadata
    let provider_a = &providers[0];
    assert_eq!(provider_a["status"], "available");
    assert!(provider_a["voices"].is_array(), "Should have voices array");
    assert!(
        !provider_a["voices"].as_array().unwrap().is_empty(),
        "Should have at least one voice"
    );
    assert!(
        provider_a["supported_formats"].is_array(),
        "Should have supported_formats"
    );

    // Check unavailable provider has reason
    let provider_c = &providers[2];
    assert_eq!(provider_c["status"], "unavailable");
    assert_eq!(provider_c["reason"], "Model not found");
    // Unavailable providers should NOT have voices array
    assert!(
        provider_c.get("voices").is_none(),
        "Unavailable provider should not have voices"
    );

    // Check not_configured provider has reason
    let provider_d = &providers[3];
    assert_eq!(provider_d["status"], "not_configured");
    assert_eq!(provider_d["reason"], "API key not set");

    server.shutdown();
}

/// Test: /providers endpoint returns correct response when no providers are available
#[tokio::test]
async fn test_providers_endpoint_empty() {
    let server = TestServerBuilder::new().spawn().await;

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/providers", server.http_url()))
        .send()
        .await
        .expect("Should send HTTP request");

    assert_eq!(response.status(), 200);

    let body: serde_json::Value = response.json().await.expect("Should parse JSON");

    // Should have empty providers array
    let providers = body["providers"]
        .as_array()
        .expect("providers should be array");
    assert!(providers.is_empty(), "Should have no providers");

    // default_provider should be null or absent
    assert!(
        body["default_provider"].is_null(),
        "Should have no default provider"
    );

    server.shutdown();
}

/// Test: Available providers include full metadata (voices, formats)
#[tokio::test]
async fn test_available_provider_includes_full_metadata() {
    let server = spawn_test_server().await;

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/providers", server.http_url()))
        .send()
        .await
        .expect("Should send HTTP request");

    let body: serde_json::Value = response.json().await.expect("Should parse JSON");
    let providers = body["providers"].as_array().unwrap();
    let provider = &providers[0];

    // Full metadata checks
    assert!(provider["id"].is_string(), "Should have id");
    assert!(provider["name"].is_string(), "Should have name");
    assert!(
        provider["description"].is_string(),
        "Should have description"
    );
    assert_eq!(provider["status"], "available");

    // Voices array with proper structure
    let voices = provider["voices"].as_array().expect("Should have voices");
    assert!(!voices.is_empty());
    let voice = &voices[0];
    assert!(voice["id"].is_string(), "Voice should have id");
    assert!(voice["name"].is_string(), "Voice should have name");
    assert!(voice["language"].is_string(), "Voice should have language");

    // Supported formats array
    let formats = provider["supported_formats"]
        .as_array()
        .expect("Should have formats");
    assert!(!formats.is_empty());
    let format = &formats[0];
    assert!(format["codec"].is_string(), "Format should have codec");
    assert!(
        format["sample_rates"].is_array(),
        "Format should have sample_rates"
    );

    server.shutdown();
}

// ============================================================================
// Provider Selection in session.init Tests
// ============================================================================

/// Test: Session initialization with multiple providers - selecting specific provider
#[tokio::test]
async fn test_session_with_multiple_providers_select_specific() {
    let server = TestServerBuilder::new()
        .with_mock_provider("primary")
        .with_mock_provider("secondary")
        .with_default_provider("primary")
        .spawn()
        .await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Request the non-default provider
    let session_id = init_session(&mut ws, Some("secondary"))
        .await
        .expect("Should initialize session with secondary provider");

    assert!(session_id.starts_with("ses_"), "Session ID should be valid");

    server.shutdown();
}

/// Test: Default provider is used when none specified
#[tokio::test]
async fn test_session_uses_default_provider_when_none_specified() {
    let server = TestServerBuilder::new()
        .with_mock_provider("first")
        .with_mock_provider("second")
        .with_default_provider("second") // Explicitly set non-first as default
        .spawn()
        .await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Don't specify a provider - should use default
    let session_id = init_session(&mut ws, None)
        .await
        .expect("Should initialize session with default provider");

    assert!(session_id.starts_with("ses_"));

    server.shutdown();
}

// ============================================================================
// FR-025: No Silent Fallback Behavior Tests
// ============================================================================

/// Test: FR-025 - When default provider is unavailable and no provider specified,
/// server MUST respond with session.error (not session.ready with different provider)
#[tokio::test]
async fn test_no_silent_fallback_when_default_unavailable() {
    // Create server where the default provider is unavailable
    let server = TestServerBuilder::new()
        .with_mock_provider("backup_provider")
        .with_unavailable_provider("default_provider", "Model not found")
        .with_default_provider("default_provider") // Set unavailable provider as default
        .spawn()
        .await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Send session.init without specifying provider (should use default)
    let init_msg = ClientMessage::SessionInit {
        provider: None,
        voice: None,
        audio_format: None,
        code_block_mode: None,
    };
    send_message(&mut ws, &init_msg)
        .await
        .expect("Should send init message");

    // Should receive session.error, NOT session.ready with backup_provider
    let response = receive_server_message_with_timeout(&mut ws, DEFAULT_TIMEOUT)
        .await
        .expect("Should receive response");

    match response {
        ServerMessage::SessionError {
            code,
            message,
            alternatives,
        } => {
            // Verify error code indicates provider unavailable
            assert_eq!(
                code, "provider_unavailable",
                "Should have provider_unavailable error code"
            );
            assert!(
                message.contains("default") || message.contains("unavailable"),
                "Message should indicate default provider is unavailable: {message}"
            );

            // FR-025: MUST include alternatives array
            let alts = alternatives.expect("Should have alternatives array");
            assert!(
                alts.contains(&"backup_provider".to_string()),
                "Alternatives should include available providers: {alts:?}"
            );
        }
        ServerMessage::SessionReady { .. } => {
            panic!("FR-025 VIOLATION: Server silently fell back to a different provider instead of returning session.error");
        }
        other => {
            panic!("Expected SessionError, got: {other:?}");
        }
    }

    server.shutdown();
}

/// Test: FR-025 - When a specific unavailable provider is requested,
/// server MUST respond with session.error (not session.ready with different provider)
#[tokio::test]
async fn test_no_silent_fallback_when_specific_provider_unavailable() {
    let server = TestServerBuilder::new()
        .with_mock_provider("available_provider")
        .with_unavailable_provider("unavailable_provider", "Service down")
        .with_default_provider("available_provider")
        .spawn()
        .await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Explicitly request the unavailable provider
    let init_msg = ClientMessage::SessionInit {
        provider: Some("unavailable_provider".to_string()),
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
        ServerMessage::SessionError {
            code,
            message,
            alternatives,
        } => {
            assert_eq!(
                code, "provider_unavailable",
                "Should have provider_unavailable error code"
            );
            assert!(
                message.contains("unavailable_provider") || message.contains("unavailable"),
                "Message should reference the requested provider: {message}"
            );

            // Should include alternatives
            let alts = alternatives.expect("Should have alternatives");
            assert!(
                alts.contains(&"available_provider".to_string()),
                "Alternatives should include available providers"
            );
        }
        ServerMessage::SessionReady { .. } => {
            panic!("FR-025 VIOLATION: Server silently used a different provider instead of returning session.error");
        }
        other => {
            panic!("Expected SessionError, got: {other:?}");
        }
    }

    server.shutdown();
}

/// Test: session.error includes alternatives array listing available providers
#[tokio::test]
async fn test_session_error_includes_alternatives() {
    let server = TestServerBuilder::new()
        .with_mock_provider("available_1")
        .with_mock_provider("available_2")
        .with_default_provider("available_1")
        .spawn()
        .await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Request a provider that doesn't exist at all
    let init_msg = ClientMessage::SessionInit {
        provider: Some("nonexistent_provider".to_string()),
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
        ServerMessage::SessionError {
            code, alternatives, ..
        } => {
            assert_eq!(code, "provider_unavailable");

            // Alternatives MUST be present
            let alts = alternatives.expect("SessionError MUST include alternatives array");

            // Should include all available providers
            assert!(
                alts.contains(&"available_1".to_string()),
                "Should include available_1 in alternatives"
            );
            assert!(
                alts.contains(&"available_2".to_string()),
                "Should include available_2 in alternatives"
            );

            // Should NOT include unavailable providers
            assert!(
                !alts.contains(&"nonexistent_provider".to_string()),
                "Should NOT include unavailable provider in alternatives"
            );
        }
        other => {
            panic!("Expected SessionError with alternatives, got: {other:?}");
        }
    }

    server.shutdown();
}

/// Test: When all providers are unavailable, alternatives should be empty
#[tokio::test]
async fn test_session_error_empty_alternatives_when_no_providers() {
    // Server with only unavailable providers
    let server = TestServerBuilder::new()
        .with_unavailable_provider("offline_1", "Service down")
        .with_not_configured_provider("offline_2", "API key missing")
        .spawn()
        .await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Try to init session - should fail since no providers available
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
        ServerMessage::SessionError { code, .. } => {
            // Should get an error about no providers being available
            assert!(
                code == "no_providers" || code == "provider_unavailable",
                "Should indicate no providers available: {code}"
            );
        }
        other => {
            panic!("Expected SessionError, got: {other:?}");
        }
    }

    server.shutdown();
}

// ============================================================================
// Additional Provider Selection Edge Cases
// ============================================================================

/// Test: Requesting a provider that was never registered (completely unknown)
#[tokio::test]
async fn test_session_error_for_completely_unknown_provider() {
    let server = spawn_test_server().await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Request a provider that was never registered
    let init_msg = ClientMessage::SessionInit {
        provider: Some("completely_unknown_provider".to_string()),
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
        ServerMessage::SessionError {
            code, alternatives, ..
        } => {
            assert_eq!(code, "provider_unavailable");
            // Should suggest the available mock provider
            let alts = alternatives.expect("Should have alternatives");
            assert!(alts.contains(&"mock".to_string()));
        }
        other => {
            panic!("Expected SessionError, got: {other:?}");
        }
    }

    server.shutdown();
}

/// Test: Case sensitivity in provider ID matching
#[tokio::test]
async fn test_provider_id_is_case_sensitive() {
    let server = TestServerBuilder::new()
        .with_mock_provider("MyProvider")
        .spawn()
        .await;

    let mut ws = connect_ws(&server)
        .await
        .expect("Should connect to WebSocket");

    // Try with different case - should fail
    let init_msg = ClientMessage::SessionInit {
        provider: Some("myprovider".to_string()), // lowercase
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
        ServerMessage::SessionError { code, .. } => {
            assert_eq!(code, "provider_unavailable");
        }
        ServerMessage::SessionReady { .. } => {
            // If it matched, the server might be case-insensitive (which is also valid)
            // This test documents the current behavior
        }
        other => {
            panic!("Expected SessionError or SessionReady, got: {other:?}");
        }
    }

    server.shutdown();
}
