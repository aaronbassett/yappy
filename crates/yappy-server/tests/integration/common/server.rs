//! Test server helpers for integration tests
//!
//! Provides utilities to spawn a test server on a random port
//! and connect to it via WebSocket.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::timeout;

use yappy_core::{
    config::{BufferConfigToml, Config, ProvidersConfig, ServerConfig},
    provider::{ProviderId, ProviderStatus},
    TtsProvider,
};
use yappy_server::{create_router, AppState, ProviderRegistry};

use super::mock::MockTtsProvider;

/// A running test server instance
pub struct TestServer {
    /// The socket address the server is listening on
    pub addr: SocketAddr,
    /// Shutdown signal sender
    shutdown_tx: Option<oneshot::Sender<()>>,
    /// Server task join handle
    _handle: tokio::task::JoinHandle<()>,
}

impl TestServer {
    /// Get the WebSocket URL for this server
    pub fn ws_url(&self) -> String {
        format!("ws://{}/ws", self.addr)
    }

    /// Get the HTTP base URL for this server
    pub fn http_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Connect to this server's WebSocket endpoint
    pub async fn connect_ws(
        &self,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        connect_ws(self)
            .await
            .expect("Failed to connect to WebSocket")
    }

    /// Shutdown the test server gracefully
    pub fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // Send shutdown signal if not already sent
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// Builder for configuring and spawning a test server
pub struct TestServerBuilder {
    /// Provider registry
    registry: ProviderRegistry,
    /// Default provider ID
    default_provider: Option<String>,
}

impl Default for TestServerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TestServerBuilder {
    /// Create a new test server builder
    pub fn new() -> Self {
        Self {
            registry: ProviderRegistry::new(),
            default_provider: None,
        }
    }

    /// Add a mock provider with default configuration
    pub fn with_mock_provider(mut self, id: &str) -> Self {
        let provider = MockTtsProvider::new(id);
        let provider_id = ProviderId::new(id);
        self.registry.register(provider);
        self.registry
            .record_status(provider_id, ProviderStatus::Available);
        if self.default_provider.is_none() {
            self.default_provider = Some(id.to_string());
        }
        self
    }

    /// Add a custom mock provider
    pub fn with_provider(mut self, provider: MockTtsProvider) -> Self {
        let id = provider.metadata().id.0;
        let provider_id = ProviderId::new(&id);
        self.registry.register(provider);
        self.registry
            .record_status(provider_id, ProviderStatus::Available);
        if self.default_provider.is_none() {
            self.default_provider = Some(id);
        }
        self
    }

    /// Add a mock provider with a specific voice
    pub fn with_mock_provider_and_voice(mut self, id: &str, voice_id: &str) -> Self {
        let provider = MockTtsProvider::with_voice(id, voice_id);
        let provider_id = ProviderId::new(id);
        self.registry.register(provider);
        self.registry
            .record_status(provider_id, ProviderStatus::Available);
        if self.default_provider.is_none() {
            self.default_provider = Some(id.to_string());
        }
        self
    }

    /// Set the default provider
    pub fn with_default_provider(mut self, id: &str) -> Self {
        self.default_provider = Some(id.to_string());
        self
    }

    /// Register an unavailable provider status (without actual provider implementation)
    ///
    /// This records a provider status for the `/providers` endpoint without registering
    /// an actual provider implementation. Useful for testing error scenarios.
    pub fn with_unavailable_provider(mut self, id: &str, reason: &str) -> Self {
        self.registry.record_status(
            ProviderId::new(id),
            ProviderStatus::Unavailable {
                reason: reason.to_string(),
            },
        );
        self
    }

    /// Register a not-configured provider status
    ///
    /// This records a provider as not configured (e.g., missing API key).
    pub fn with_not_configured_provider(mut self, id: &str, reason: &str) -> Self {
        self.registry.record_status(
            ProviderId::new(id),
            ProviderStatus::NotConfigured {
                reason: reason.to_string(),
            },
        );
        self
    }

    /// Build and spawn the test server
    ///
    /// Returns the test server handle which includes the bound address.
    pub async fn spawn(mut self) -> TestServer {
        // Set default provider if specified
        if let Some(ref default_id) = self.default_provider {
            self.registry.set_default(ProviderId::new(default_id));
        }

        // Create minimal test configuration
        // Use a long flush timeout (5 seconds) to prevent auto-flush during tests,
        // preserving the original test semantics where buffers only flush on text.done.
        // Tests that want to test flush timeout behavior should use a custom config.
        let config = Config {
            server: ServerConfig::default(),
            providers: ProvidersConfig {
                default: self.default_provider.clone().unwrap_or_default(),
                openai: None,
                kokoro: None,
                avspeech: None,
            },
            buffer: BufferConfigToml {
                flush_timeout_ms: 5000, // 5 seconds - long enough to prevent auto-flush in tests
                max_size_bytes: 4096,
            },
        };

        // Create application state
        let state = AppState::new(config, self.registry);

        // Create the router
        let router = create_router(state);

        // Bind to a random available port
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind test server");

        let addr = listener.local_addr().expect("Failed to get local address");

        // Create shutdown channel
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        // Spawn the server
        let handle = tokio::spawn(async move {
            let server = axum::serve(listener, router).with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            });

            if let Err(e) = server.await {
                eprintln!("Test server error: {e}");
            }
        });

        // Give the server a moment to start
        tokio::time::sleep(Duration::from_millis(10)).await;

        TestServer {
            addr,
            shutdown_tx: Some(shutdown_tx),
            _handle: handle,
        }
    }
}

/// Helper to spawn a test server with a single mock provider
pub async fn spawn_test_server() -> TestServer {
    TestServerBuilder::new()
        .with_mock_provider("mock")
        .spawn()
        .await
}

/// Helper to spawn a test server with no providers (for error testing)
pub async fn spawn_empty_server() -> TestServer {
    TestServerBuilder::new().spawn().await
}

/// Connect to a test server's WebSocket endpoint
///
/// Returns the WebSocket stream for sending and receiving messages.
pub async fn connect_ws(
    server: &TestServer,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tokio_tungstenite::tungstenite::Error,
> {
    let url = server.ws_url();
    let (ws_stream, _response) = tokio_tungstenite::connect_async(&url).await?;
    Ok(ws_stream)
}

/// Connect to a test server with timeout
#[allow(dead_code)]
pub async fn connect_ws_with_timeout(
    server: &TestServer,
    timeout_duration: Duration,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    String,
> {
    timeout(timeout_duration, connect_ws(server))
        .await
        .map_err(|_| "Connection timeout".to_string())?
        .map_err(|e| format!("WebSocket connection error: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_spawn_test_server() {
        let server = spawn_test_server().await;

        // Server should have a valid address
        assert!(server.addr.port() > 0);

        // WebSocket URL should be well-formed
        let url = server.ws_url();
        assert!(url.starts_with("ws://127.0.0.1:"));
        assert!(url.ends_with("/ws"));

        // HTTP URL should be well-formed
        let http_url = server.http_url();
        assert!(http_url.starts_with("http://127.0.0.1:"));

        // Server should shutdown gracefully
        server.shutdown();
    }

    #[tokio::test]
    async fn test_spawn_empty_server() {
        let server = spawn_empty_server().await;
        assert!(server.addr.port() > 0);
        server.shutdown();
    }

    #[tokio::test]
    async fn test_connect_ws() {
        let server = spawn_test_server().await;

        // Should be able to connect
        let ws = connect_ws(&server).await;
        assert!(ws.is_ok());

        server.shutdown();
    }
}
