//! Axum router and HTTP handlers
//!
//! This module provides the HTTP routing setup for the Yappy TTS server:
//! - [`create_router`] - Creates the Axum router with all endpoints
//! - Handler functions for `/health`, `/providers`, and `/ws` endpoints

use std::collections::HashMap;

use axum::{
    extract::State,
    http::{HeaderName, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tower_http::{
    propagate_header::PropagateHeaderLayer,
    request_id::{MakeRequestUuid, SetRequestIdLayer},
};
use yappy_core::ProviderStatus;

use crate::handlers::providers::list_providers;
use crate::state::AppState;
use crate::ws::ws_upgrade_handler;

/// X-Request-ID header name
const X_REQUEST_ID: &str = "x-request-id";

/// Create the Axum router with all endpoints and middleware
///
/// Sets up the following routes:
/// - `GET /health` - Health check endpoint
/// - `GET /providers` - List available TTS providers
/// - `WS /ws` - WebSocket endpoint for streaming TTS
///
/// Middleware applied:
/// - `SetRequestIdLayer` - Generates X-Request-ID for each request
/// - `PropagateHeaderLayer` - Copies X-Request-ID to response headers
///
/// # Arguments
///
/// * `state` - Application state to be shared across handlers
///
/// # Example
///
/// ```ignore
/// let state = AppState::new(config, registry);
/// let router = create_router(state);
/// let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
/// axum::serve(listener, router).await?;
/// ```
pub fn create_router(state: AppState) -> Router {
    let x_request_id = HeaderName::from_static(X_REQUEST_ID);

    Router::new()
        .route("/health", get(health_handler))
        .route("/providers", get(list_providers))
        .route("/ws", any(ws_upgrade_handler))
        .layer(PropagateHeaderLayer::new(x_request_id.clone()))
        .layer(SetRequestIdLayer::new(x_request_id, MakeRequestUuid))
        .with_state(state)
}

/// Health check response
///
/// Returned by `GET /health` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    /// Overall status: "ok", "degraded", or "unhealthy"
    pub status: HealthStatus,

    /// Server uptime in seconds
    pub uptime_secs: u64,

    /// Status of each registered provider
    pub providers: HashMap<String, ProviderStatusResponse>,
}

/// Overall health status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    /// All providers are available
    Ok,
    /// Some providers are available, some are not
    Degraded,
    /// No providers are available
    Unhealthy,
}

/// Provider status in health response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderStatusResponse {
    /// Provider status: "available", `not_configured`, or "unavailable"
    #[serde(flatten)]
    pub status: ProviderStatus,
}

/// Health check handler
///
/// Returns the server health status including uptime and provider availability.
/// Reports on ALL compiled-in providers, including those that failed to initialize
/// (`NotConfigured` or `Unavailable`), to give operators complete visibility.
///
/// For providers marked as `Available` in the initialization status, this handler
/// performs a dynamic health check to verify current availability. Providers that
/// were `NotConfigured` or `Unavailable` at startup retain their initialization status.
///
/// # Response
///
/// - `200 OK` - Server is healthy (at least one provider available)
/// - `503 Service Unavailable` - No providers are available
///
/// # Example Response
///
/// ```json
/// {
///   "status": "degraded",
///   "uptime_secs": 3600,
///   "providers": {
///     "kokoro": {"status": "available"},
///     "openai": {"status": "not_configured", "reason": "API key not set"},
///     "avspeech": {"status": "unavailable", "reason": "macOS only"}
///   }
/// }
/// ```
pub async fn health_handler(State(state): State<AppState>) -> Response {
    let uptime_secs = state.uptime().as_secs();
    let registry = state.providers();

    // Build provider status map from all known providers (not just registered ones)
    // This ensures NotConfigured and Unavailable providers are included (fixes #18)
    let mut providers = HashMap::new();
    let mut available_count = 0;
    let mut total_count = 0;

    for (provider_id, init_status) in registry.all_statuses() {
        total_count += 1;
        let id_string = provider_id.0.clone();

        // For providers that initialized successfully, perform a dynamic health check
        // For NotConfigured/Unavailable providers, use the recorded initialization status
        let status = if init_status.is_available() {
            // Provider was available at startup - check current health
            if let Some(provider) = registry.get(provider_id) {
                provider.health_check().await
            } else {
                // Status says available but provider not in registry - unexpected state
                ProviderStatus::Unavailable {
                    reason: "Provider registered but not found".to_string(),
                }
            }
        } else {
            // Provider was NotConfigured or Unavailable at startup - use that status
            init_status.clone()
        };

        if status.is_available() {
            available_count += 1;
        }
        providers.insert(id_string, ProviderStatusResponse { status });
    }

    // Determine overall health status
    let status = if total_count == 0 || available_count == 0 {
        HealthStatus::Unhealthy
    } else if available_count == total_count {
        HealthStatus::Ok
    } else {
        HealthStatus::Degraded
    };

    let response = HealthResponse {
        status,
        uptime_secs,
        providers,
    };

    // Return 503 if unhealthy
    if status == HealthStatus::Unhealthy {
        (StatusCode::SERVICE_UNAVAILABLE, Json(response)).into_response()
    } else {
        Json(response).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    use yappy_core::{
        audio::{AudioFormat, AudioStream},
        config::Config,
        error::ProviderError,
        provider::{ProviderId, ProviderMetadata},
        session::VoiceConfig,
        TtsProvider,
    };

    use crate::handlers::providers::ProvidersResponse;
    use crate::state::ProviderRegistry;

    /// Mock TTS provider for testing
    struct MockProvider {
        id: String,
        name: String,
        status: ProviderStatus,
    }

    impl MockProvider {
        fn new(id: &str, name: &str) -> Self {
            Self {
                id: id.to_string(),
                name: name.to_string(),
                status: ProviderStatus::Available,
            }
        }

        fn unavailable(id: &str, name: &str, reason: &str) -> Self {
            Self {
                id: id.to_string(),
                name: name.to_string(),
                status: ProviderStatus::Unavailable {
                    reason: reason.to_string(),
                },
            }
        }
    }

    #[async_trait]
    impl TtsProvider for MockProvider {
        fn metadata(&self) -> ProviderMetadata {
            ProviderMetadata {
                id: ProviderId::new(&self.id),
                name: self.name.clone(),
                description: format!("Mock {} provider", self.name),
                voices: vec![],
                supported_formats: vec![AudioFormat::default()],
                options_schema: None,
            }
        }

        async fn health_check(&self) -> ProviderStatus {
            self.status.clone()
        }

        async fn synthesize(
            &self,
            _text: &str,
            _voice: &VoiceConfig,
            _format: AudioFormat,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<AudioStream, ProviderError> {
            unimplemented!("Mock provider does not synthesize")
        }
    }

    fn create_test_config() -> Config {
        Config {
            server: yappy_core::config::ServerConfig::default(),
            providers: yappy_core::config::ProvidersConfig {
                default: "test".to_string(),
                openai: None,
                kokoro: None,
                avspeech: None,
                max_concurrent_synthesis: 4,
            },
            buffer: yappy_core::config::BufferConfigToml::default(),
        }
    }

    fn create_test_state_with_provider(provider: impl TtsProvider + 'static) -> AppState {
        let config = create_test_config();
        let mut registry = ProviderRegistry::new();
        let id = ProviderId::new("test");
        registry.register(provider);
        registry.record_status(id.clone(), ProviderStatus::Available);
        registry.set_default(id);
        AppState::new(config, registry)
    }

    fn create_empty_state() -> AppState {
        let config = create_test_config();
        let registry = ProviderRegistry::new();
        AppState::new(config, registry)
    }

    #[tokio::test]
    async fn test_health_ok_with_available_provider() {
        let state = create_test_state_with_provider(MockProvider::new("test", "Test Provider"));
        let app = create_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let health: HealthResponse = serde_json::from_slice(&body).unwrap();

        assert_eq!(health.status, HealthStatus::Ok);
        assert!(health.providers.contains_key("test"));
    }

    #[tokio::test]
    async fn test_health_unhealthy_no_providers() {
        let state = create_empty_state();
        let app = create_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let health: HealthResponse = serde_json::from_slice(&body).unwrap();

        assert_eq!(health.status, HealthStatus::Unhealthy);
    }

    #[tokio::test]
    async fn test_health_degraded_with_mixed_providers() {
        let config = create_test_config();
        let mut registry = ProviderRegistry::new();
        registry.register(MockProvider::new("available", "Available"));
        registry.record_status(ProviderId::new("available"), ProviderStatus::Available);
        registry.register(MockProvider::unavailable(
            "broken",
            "Broken",
            "Test failure",
        ));
        registry.record_status(
            ProviderId::new("broken"),
            ProviderStatus::Unavailable {
                reason: "Test failure".to_string(),
            },
        );
        let state = AppState::new(config, registry);
        let app = create_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let health: HealthResponse = serde_json::from_slice(&body).unwrap();

        assert_eq!(health.status, HealthStatus::Degraded);
    }

    /// Test that /health includes `NotConfigured` providers that weren't registered
    /// (fixes #18: /health endpoint omits NotConfigured/Unavailable providers)
    #[tokio::test]
    async fn test_health_includes_not_configured_providers() {
        let config = create_test_config();
        let mut registry = ProviderRegistry::new();

        // Register one available provider
        registry.register(MockProvider::new("kokoro", "Kokoro"));
        registry.record_status(ProviderId::new("kokoro"), ProviderStatus::Available);
        registry.set_default(ProviderId::new("kokoro"));

        // Record NotConfigured status for a provider that's NOT registered
        // (simulates provider that failed config validation at startup)
        registry.record_status(
            ProviderId::new("openai"),
            ProviderStatus::NotConfigured {
                reason: "API key not set".to_string(),
            },
        );

        // Record Unavailable status for another provider that's NOT registered
        // (simulates provider that's not available on this platform)
        registry.record_status(
            ProviderId::new("avspeech"),
            ProviderStatus::Unavailable {
                reason: "macOS only".to_string(),
            },
        );

        let state = AppState::new(config, registry);
        let app = create_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let health: HealthResponse = serde_json::from_slice(&body).unwrap();

        // Should be degraded (1 available, 2 not available)
        assert_eq!(health.status, HealthStatus::Degraded);

        // All 3 providers should be in the response
        assert_eq!(health.providers.len(), 3);
        assert!(health.providers.contains_key("kokoro"));
        assert!(health.providers.contains_key("openai"));
        assert!(health.providers.contains_key("avspeech"));

        // Verify status values
        assert!(health.providers["kokoro"].status.is_available());

        match &health.providers["openai"].status {
            ProviderStatus::NotConfigured { reason } => {
                assert_eq!(reason, "API key not set");
            }
            other => panic!("Expected NotConfigured, got {other:?}"),
        }

        match &health.providers["avspeech"].status {
            ProviderStatus::Unavailable { reason } => {
                assert_eq!(reason, "macOS only");
            }
            other => panic!("Expected Unavailable, got {other:?}"),
        }
    }

    /// Test that /health returns unhealthy when only `NotConfigured` providers exist
    #[tokio::test]
    async fn test_health_unhealthy_with_only_not_configured_providers() {
        let config = create_test_config();
        let mut registry = ProviderRegistry::new();

        // Only record NotConfigured status - no actual providers registered
        registry.record_status(
            ProviderId::new("openai"),
            ProviderStatus::NotConfigured {
                reason: "API key not set".to_string(),
            },
        );

        let state = AppState::new(config, registry);
        let app = create_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Should return 503 since no providers are available
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let health: HealthResponse = serde_json::from_slice(&body).unwrap();

        assert_eq!(health.status, HealthStatus::Unhealthy);

        // The NotConfigured provider should still be in the response
        assert_eq!(health.providers.len(), 1);
        assert!(health.providers.contains_key("openai"));
    }

    #[tokio::test]
    async fn test_providers_list() {
        let state = create_test_state_with_provider(MockProvider::new("test", "Test Provider"));
        let app = create_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/providers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let providers: ProvidersResponse = serde_json::from_slice(&body).unwrap();

        assert_eq!(providers.providers.len(), 1);
        assert_eq!(providers.default_provider, Some("test".to_string()));
    }

    #[tokio::test]
    async fn test_ws_requires_upgrade() {
        // Non-WebSocket requests to /ws should fail
        // The WebSocketUpgrade extractor returns 400 when upgrade headers are missing
        let state = create_empty_state();
        let app = create_router(state);

        let response = app
            .oneshot(Request::builder().uri("/ws").body(Body::empty()).unwrap())
            .await
            .unwrap();

        // Without proper WebSocket upgrade headers, the request should be rejected
        // Axum's WebSocketUpgrade returns a 400 Bad Request when headers are missing
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_request_id_header_propagated() {
        let state = create_empty_state();
        let app = create_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // The X-Request-ID header should be present in the response
        assert!(response.headers().contains_key("x-request-id"));
    }
}
