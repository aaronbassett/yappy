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
use yappy_core::{ProviderMetadata, ProviderStatus};

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
        .route("/providers", get(providers_handler))
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

/// Provider list response
///
/// Returned by `GET /providers` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvidersResponse {
    /// List of all provider metadata
    pub providers: Vec<ProviderWithStatus>,

    /// ID of the default provider
    pub default_provider: Option<String>,
}

/// Provider metadata with current status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderWithStatus {
    /// Provider metadata (id, name, description, voices, etc.)
    #[serde(flatten)]
    pub metadata: ProviderMetadata,

    /// Current provider status
    #[serde(flatten)]
    pub status: ProviderStatus,
}

/// Health check handler
///
/// Returns the server health status including uptime and provider availability.
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
///   "status": "ok",
///   "uptime_secs": 3600,
///   "providers": {
///     "kokoro": {"status": "available"}
///   }
/// }
/// ```
pub async fn health_handler(State(state): State<AppState>) -> Response {
    let uptime_secs = state.uptime().as_secs();
    let registry = state.providers();

    // Build provider status map
    let mut providers = HashMap::new();
    let mut available_count = 0;
    let mut total_count = 0;

    for metadata in registry.list() {
        total_count += 1;
        let provider_id = metadata.id.0.clone();

        // Get the actual provider to check its health
        if let Some(provider) = registry.get(&metadata.id) {
            let status = provider.health_check().await;
            if status.is_available() {
                available_count += 1;
            }
            providers.insert(provider_id, ProviderStatusResponse { status });
        }
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

/// List providers handler
///
/// Returns metadata for all registered TTS providers including their
/// capabilities, voices, and supported audio formats.
///
/// # Response
///
/// Always returns `200 OK` with the provider list.
///
/// # Example Response
///
/// ```json
/// {
///   "providers": [
///     {
///       "id": "kokoro",
///       "name": "Kokoro 82M",
///       "status": "available",
///       "voices": [...]
///     }
///   ],
///   "default_provider": "kokoro"
/// }
/// ```
pub async fn providers_handler(State(state): State<AppState>) -> Json<ProvidersResponse> {
    let registry = state.providers();

    // Collect provider metadata with status
    let mut providers = Vec::new();
    for metadata in registry.list() {
        let status = if let Some(provider) = registry.get(&metadata.id) {
            provider.health_check().await
        } else {
            ProviderStatus::Unavailable {
                reason: "Provider not found".to_string(),
            }
        };

        providers.push(ProviderWithStatus { metadata, status });
    }

    let default_provider = registry.default_provider().map(|id| id.0);

    Json(ProvidersResponse {
        providers,
        default_provider,
    })
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
        provider::ProviderId,
        session::VoiceConfig,
        TtsProvider,
    };

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
            },
            buffer: yappy_core::config::BufferConfigToml::default(),
        }
    }

    fn create_test_state_with_provider(provider: impl TtsProvider + 'static) -> AppState {
        let config = create_test_config();
        let mut registry = ProviderRegistry::new();
        registry.register(provider);
        registry.set_default(ProviderId::new("test"));
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
        registry.register(MockProvider::unavailable(
            "broken",
            "Broken",
            "Test failure",
        ));
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
        assert_eq!(providers.providers[0].metadata.id.0, "test");
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
