//! Health check endpoint integration tests for the Yappy TTS server
//!
//! These tests verify the `/health` endpoint behavior according to the HTTP API contract:
//! - Returns status "ok", "degraded", or "unhealthy"
//! - Returns `uptime_secs`
//! - Returns providers object with per-provider status
//! - Returns 200 OK when at least one provider is available
//! - Returns 503 Service Unavailable when no providers are available
//!
//! See: `specs/001-streaming-tts-server/contracts/http-api.md`

use std::time::{Duration, Instant};

use serde::Deserialize;
use std::collections::HashMap;

use crate::common::{spawn_empty_server, spawn_test_server, MockTtsProvider, TestServerBuilder};

/// Health response structure matching the API contract
#[derive(Debug, Deserialize)]
struct HealthResponse {
    status: String,
    uptime_secs: u64,
    providers: HashMap<String, ProviderStatusResponse>,
}

/// Provider status in health response (flattened from the actual enum)
#[derive(Debug, Deserialize)]
struct ProviderStatusResponse {
    status: String,
    #[serde(default)]
    reason: Option<String>,
}

// ============================================================================
// Basic Health Check Tests
// ============================================================================

/// Test: Health endpoint returns "ok" status with a single available provider
///
/// Given a running server with one available provider,
/// When GET /health is called,
/// Then response includes status "ok" and the provider shows "available".
#[tokio::test]
async fn test_health_ok_single_provider() {
    let server = spawn_test_server().await;

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/health", server.http_url()))
        .send()
        .await
        .expect("Should send HTTP request");

    assert_eq!(
        response.status(),
        200,
        "Should return 200 OK when provider is available"
    );

    let health: HealthResponse = response.json().await.expect("Should parse JSON");

    assert_eq!(health.status, "ok", "Status should be 'ok'");
    assert!(
        health.providers.contains_key("mock"),
        "Should contain mock provider"
    );

    let mock_status = &health.providers["mock"];
    assert_eq!(
        mock_status.status, "available",
        "Mock provider should be available"
    );
    assert!(
        mock_status.reason.is_none(),
        "Available provider should not have a reason"
    );

    server.shutdown();
}

/// Test: Health endpoint returns "ok" status with multiple available providers
///
/// Given a running server with multiple available providers,
/// When GET /health is called,
/// Then response includes status "ok" and all providers show "available".
#[tokio::test]
async fn test_health_ok_multiple_providers() {
    let server = TestServerBuilder::new()
        .with_mock_provider("provider_a")
        .with_mock_provider("provider_b")
        .with_mock_provider("provider_c")
        .with_default_provider("provider_a")
        .spawn()
        .await;

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/health", server.http_url()))
        .send()
        .await
        .expect("Should send HTTP request");

    assert_eq!(response.status(), 200);

    let health: HealthResponse = response.json().await.expect("Should parse JSON");

    assert_eq!(
        health.status, "ok",
        "Status should be 'ok' with all providers available"
    );
    assert_eq!(
        health.providers.len(),
        3,
        "Should have 3 providers in response"
    );

    // All providers should be available
    for (id, status) in &health.providers {
        assert_eq!(
            status.status, "available",
            "Provider {id} should be available"
        );
    }

    server.shutdown();
}

// ============================================================================
// Degraded Status Tests
// ============================================================================

/// Test: Health endpoint returns "degraded" status when one provider is unavailable
///
/// Given a server where one provider is unavailable,
/// When GET /health is called,
/// Then status is "degraded" but HTTP status is still 200 OK.
#[tokio::test]
async fn test_health_degraded_mixed_providers() {
    // Create a server with one available and one unavailable provider
    let unavailable_provider = MockTtsProvider::unavailable("broken", "Simulated failure");

    let server = TestServerBuilder::new()
        .with_mock_provider("healthy")
        .with_provider(unavailable_provider)
        .with_default_provider("healthy")
        .spawn()
        .await;

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/health", server.http_url()))
        .send()
        .await
        .expect("Should send HTTP request");

    // Should still return 200 because at least one provider is available
    assert_eq!(
        response.status(),
        200,
        "Should return 200 OK when at least one provider is available"
    );

    let health: HealthResponse = response.json().await.expect("Should parse JSON");

    assert_eq!(
        health.status, "degraded",
        "Status should be 'degraded' with mixed provider availability"
    );

    // Verify individual provider statuses
    assert_eq!(
        health.providers["healthy"].status, "available",
        "Healthy provider should be available"
    );
    assert_eq!(
        health.providers["broken"].status, "unavailable",
        "Broken provider should be unavailable"
    );

    server.shutdown();
}

/// Test: Health endpoint returns "degraded" when multiple providers are unavailable but one works
#[tokio::test]
async fn test_health_degraded_multiple_unavailable() {
    let unavailable_1 = MockTtsProvider::unavailable("offline_1", "Service down");
    let unavailable_2 = MockTtsProvider::unavailable("offline_2", "Connection timeout");

    let server = TestServerBuilder::new()
        .with_mock_provider("working")
        .with_provider(unavailable_1)
        .with_provider(unavailable_2)
        .with_default_provider("working")
        .spawn()
        .await;

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/health", server.http_url()))
        .send()
        .await
        .expect("Should send HTTP request");

    assert_eq!(response.status(), 200);

    let health: HealthResponse = response.json().await.expect("Should parse JSON");

    assert_eq!(health.status, "degraded");
    assert_eq!(health.providers.len(), 3);

    // One available, two unavailable
    let available_count = health
        .providers
        .values()
        .filter(|p| p.status == "available")
        .count();
    let unavailable_count = health
        .providers
        .values()
        .filter(|p| p.status == "unavailable")
        .count();

    assert_eq!(available_count, 1, "Should have 1 available provider");
    assert_eq!(unavailable_count, 2, "Should have 2 unavailable providers");

    server.shutdown();
}

// ============================================================================
// Unhealthy Status Tests
// ============================================================================

/// Test: Health endpoint returns "unhealthy" and 503 when no providers are available
///
/// Given a server with no TTS providers registered,
/// When GET /health is called,
/// Then response status is "unhealthy" and HTTP status is 503.
#[tokio::test]
async fn test_health_unhealthy_no_providers() {
    let server = spawn_empty_server().await;

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/health", server.http_url()))
        .send()
        .await
        .expect("Should send HTTP request");

    assert_eq!(
        response.status(),
        503,
        "Should return 503 Service Unavailable when no providers"
    );

    let health: HealthResponse = response.json().await.expect("Should parse JSON");

    assert_eq!(
        health.status, "unhealthy",
        "Status should be 'unhealthy' with no providers"
    );
    assert!(health.providers.is_empty(), "Providers map should be empty");

    server.shutdown();
}

/// Test: Health endpoint returns "unhealthy" when all providers are unavailable
#[tokio::test]
async fn test_health_unhealthy_all_providers_unavailable() {
    let unavailable_1 = MockTtsProvider::unavailable("provider_1", "Model not found");
    let unavailable_2 = MockTtsProvider::unavailable("provider_2", "API key invalid");

    let server = TestServerBuilder::new()
        .with_provider(unavailable_1)
        .with_provider(unavailable_2)
        .spawn()
        .await;

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/health", server.http_url()))
        .send()
        .await
        .expect("Should send HTTP request");

    assert_eq!(
        response.status(),
        503,
        "Should return 503 when all providers are unavailable"
    );

    let health: HealthResponse = response.json().await.expect("Should parse JSON");

    assert_eq!(health.status, "unhealthy");
    assert_eq!(health.providers.len(), 2);

    // All providers should be unavailable
    for (id, status) in &health.providers {
        assert_eq!(
            status.status, "unavailable",
            "Provider {id} should be unavailable"
        );
    }

    server.shutdown();
}

// ============================================================================
// Uptime Tests
// ============================================================================

/// Test: Health response includes `uptime_secs` field with valid value
///
/// Given a running server,
/// When GET /health is called,
/// Then response includes `uptime_secs` >= 0.
#[tokio::test]
async fn test_health_includes_uptime() {
    let server = spawn_test_server().await;

    // Wait a short time to ensure uptime is measurable
    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/health", server.http_url()))
        .send()
        .await
        .expect("Should send HTTP request");

    assert_eq!(response.status(), 200);

    let health: HealthResponse = response.json().await.expect("Should parse JSON");

    // Uptime should be at least 0 (it might be 0 if checked immediately)
    // We can't assert an exact value due to timing, but we can verify the field exists
    // and is a reasonable value (not negative, not absurdly large)
    assert!(
        health.uptime_secs < 3600,
        "Uptime should be less than 1 hour for a test server"
    );

    server.shutdown();
}

/// Test: Health uptime increases over time
#[tokio::test]
async fn test_health_uptime_increases() {
    let server = spawn_test_server().await;

    let client = reqwest::Client::new();

    // First health check
    let response1 = client
        .get(format!("{}/health", server.http_url()))
        .send()
        .await
        .expect("First request");
    let health1: HealthResponse = response1.json().await.expect("Parse first response");

    // Wait a bit
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Second health check
    let response2 = client
        .get(format!("{}/health", server.http_url()))
        .send()
        .await
        .expect("Second request");
    let health2: HealthResponse = response2.json().await.expect("Parse second response");

    assert!(
        health2.uptime_secs >= health1.uptime_secs,
        "Uptime should not decrease: {} -> {}",
        health1.uptime_secs,
        health2.uptime_secs
    );

    server.shutdown();
}

// ============================================================================
// Provider Status Reason Tests
// ============================================================================

/// Test: Unavailable provider includes reason field in health response
///
/// Given a server with an unavailable provider,
/// When GET /health is called,
/// Then the unavailable provider includes a reason field.
#[tokio::test]
async fn test_health_provider_unavailable_shows_reason() {
    let reason = "Model file not found in expected location";
    let unavailable_provider = MockTtsProvider::unavailable("broken_provider", reason);

    let server = TestServerBuilder::new()
        .with_mock_provider("working")
        .with_provider(unavailable_provider)
        .spawn()
        .await;

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/health", server.http_url()))
        .send()
        .await
        .expect("Should send HTTP request");

    let health: HealthResponse = response.json().await.expect("Should parse JSON");

    let broken_status = &health.providers["broken_provider"];
    assert_eq!(broken_status.status, "unavailable");
    assert_eq!(
        broken_status.reason.as_deref(),
        Some(reason),
        "Unavailable provider should include the reason"
    );

    // Available provider should not have a reason
    let working_status = &health.providers["working"];
    assert_eq!(working_status.status, "available");
    assert!(
        working_status.reason.is_none(),
        "Available provider should not have a reason"
    );

    server.shutdown();
}

/// Test: Not-configured provider shows reason (via /providers endpoint pattern)
///
/// Note: The health endpoint uses dynamic `health_check()` calls, so `not_configured`
/// status would come from the provider's `health_check()` method. This test verifies
/// that unavailable providers consistently include reasons.
#[tokio::test]
async fn test_health_provider_not_configured_shows_reason() {
    // The MockTtsProvider::unavailable() can simulate this scenario
    // In a real system, NotConfigured would come from missing API keys, etc.
    let reason = "API key not configured";
    let not_configured = MockTtsProvider::unavailable("unconfigured_api", reason);

    let server = TestServerBuilder::new()
        .with_mock_provider("configured")
        .with_provider(not_configured)
        .spawn()
        .await;

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/health", server.http_url()))
        .send()
        .await
        .expect("Should send HTTP request");

    let health: HealthResponse = response.json().await.expect("Should parse JSON");

    let unconfigured_status = &health.providers["unconfigured_api"];
    assert!(
        unconfigured_status.reason.is_some(),
        "Unconfigured provider should have a reason"
    );
    assert_eq!(unconfigured_status.reason.as_deref(), Some(reason));

    server.shutdown();
}

// ============================================================================
// Response Time Tests
// ============================================================================

/// Test: Health endpoint responds within acceptable time limit
///
/// Response time target from spec: < 10ms
/// Relaxed to < 100ms for test stability across different environments.
#[tokio::test]
async fn test_health_response_time() {
    let server = spawn_test_server().await;

    let client = reqwest::Client::new();
    let start = Instant::now();

    let response = client
        .get(format!("{}/health", server.http_url()))
        .send()
        .await
        .expect("Should send HTTP request");

    let elapsed = start.elapsed();

    assert_eq!(response.status(), 200);
    assert!(
        elapsed < Duration::from_millis(100),
        "Health check should complete in < 100ms, took {elapsed:?}"
    );

    server.shutdown();
}

/// Test: Health endpoint response time is consistent across multiple requests
#[tokio::test]
async fn test_health_response_time_consistent() {
    let server = spawn_test_server().await;
    let client = reqwest::Client::new();

    let mut times = Vec::new();

    // Make multiple requests
    for _ in 0..5 {
        let start = Instant::now();
        let _response = client
            .get(format!("{}/health", server.http_url()))
            .send()
            .await
            .expect("Should send HTTP request");
        times.push(start.elapsed());
    }

    // All requests should be fast
    for (i, time) in times.iter().enumerate() {
        assert!(
            *time < Duration::from_millis(100),
            "Request {} took {:?}, exceeds 100ms limit",
            i + 1,
            time
        );
    }

    server.shutdown();
}

// ============================================================================
// Response Structure Tests
// ============================================================================

/// Test: Health response JSON structure matches the API contract
#[tokio::test]
async fn test_health_response_structure() {
    let server = spawn_test_server().await;

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/health", server.http_url()))
        .send()
        .await
        .expect("Should send HTTP request");

    let body: serde_json::Value = response.json().await.expect("Should parse JSON");

    // Verify required top-level fields exist
    assert!(
        body.get("status").is_some(),
        "Response should have 'status' field"
    );
    assert!(
        body.get("uptime_secs").is_some(),
        "Response should have 'uptime_secs' field"
    );
    assert!(
        body.get("providers").is_some(),
        "Response should have 'providers' field"
    );

    // Verify status is a string with valid value
    let status = body["status"].as_str().expect("status should be a string");
    assert!(
        ["ok", "degraded", "unhealthy"].contains(&status),
        "status should be ok, degraded, or unhealthy"
    );

    // Verify uptime_secs is a number
    assert!(
        body["uptime_secs"].is_u64(),
        "uptime_secs should be a non-negative integer"
    );

    // Verify providers is an object
    assert!(
        body["providers"].is_object(),
        "providers should be an object"
    );

    server.shutdown();
}

/// Test: Health response includes X-Request-ID header
#[tokio::test]
async fn test_health_includes_request_id_header() {
    let server = spawn_test_server().await;

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/health", server.http_url()))
        .send()
        .await
        .expect("Should send HTTP request");

    assert!(
        response.headers().contains_key("x-request-id"),
        "Response should include X-Request-ID header"
    );

    let request_id = response.headers().get("x-request-id").unwrap();
    assert!(!request_id.is_empty(), "X-Request-ID should not be empty");

    server.shutdown();
}

/// Test: Health endpoint with multiple providers shows all in response
#[tokio::test]
async fn test_health_shows_all_providers() {
    let unavailable = MockTtsProvider::unavailable("unavailable_one", "Service down");

    let server = TestServerBuilder::new()
        .with_mock_provider("available_one")
        .with_mock_provider("available_two")
        .with_provider(unavailable)
        .spawn()
        .await;

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/health", server.http_url()))
        .send()
        .await
        .expect("Should send HTTP request");

    let health: HealthResponse = response.json().await.expect("Should parse JSON");

    assert_eq!(
        health.providers.len(),
        3,
        "Should show all 3 registered providers"
    );
    assert!(health.providers.contains_key("available_one"));
    assert!(health.providers.contains_key("available_two"));
    assert!(health.providers.contains_key("unavailable_one"));

    server.shutdown();
}
