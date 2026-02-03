//! Application state and provider registry
//!
//! This module provides the shared state for the Yappy TTS server:
//! - [`ProviderRegistry`] - Thread-safe registry of TTS providers
//! - [`AppState`] - Application state shared across Axum handlers

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use yappy_core::provider::ProviderId;
use yappy_core::{Config, ProviderMetadata, ProviderStatus, TtsProvider};

/// Registry holding all available TTS providers
///
/// The registry maps provider IDs to their implementations and tracks
/// which provider is the default. It is designed to be wrapped in an
/// `Arc` for thread-safe sharing across Axum handlers.
///
/// # Example
///
/// ```ignore
/// let mut registry = ProviderRegistry::new();
/// registry.register(kokoro_provider);
/// registry.set_default(ProviderId::new("kokoro"));
///
/// // Get a provider
/// if let Some(provider) = registry.get(&ProviderId::new("kokoro")) {
///     let metadata = provider.metadata();
///     println!("Using provider: {}", metadata.name);
/// }
/// ```
#[derive(Default)]
pub struct ProviderRegistry {
    /// Map of provider ID to provider implementation
    providers: HashMap<ProviderId, Arc<dyn TtsProvider + Send + Sync>>,
    /// The default provider ID
    default_provider: Option<ProviderId>,
}

impl ProviderRegistry {
    /// Create an empty provider registry
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
            default_provider: None,
        }
    }

    /// Register a TTS provider
    ///
    /// The provider's ID is extracted from its metadata. If a provider
    /// with the same ID already exists, it will be replaced.
    ///
    /// # Arguments
    ///
    /// * `provider` - The TTS provider to register
    pub fn register(&mut self, provider: impl TtsProvider + 'static) {
        let metadata = provider.metadata();
        let id = metadata.id;
        self.providers.insert(id, Arc::new(provider));
    }

    /// Get a provider by ID
    ///
    /// Returns `None` if no provider with the given ID is registered.
    ///
    /// # Arguments
    ///
    /// * `id` - The provider ID to look up
    pub fn get(&self, id: &ProviderId) -> Option<Arc<dyn TtsProvider + Send + Sync>> {
        self.providers.get(id).cloned()
    }

    /// List metadata for all registered providers
    ///
    /// Returns metadata in no particular order.
    pub fn list(&self) -> Vec<ProviderMetadata> {
        self.providers
            .values()
            .map(|p: &Arc<dyn TtsProvider + Send + Sync>| p.metadata())
            .collect()
    }

    /// Get the default provider ID
    ///
    /// Returns `None` if no default has been set or if the registry is empty.
    pub fn default_provider(&self) -> Option<ProviderId> {
        self.default_provider.clone()
    }

    /// Set the default provider
    ///
    /// # Arguments
    ///
    /// * `id` - The provider ID to set as default
    ///
    /// # Panics
    ///
    /// This method does not panic if the provider ID is not registered.
    /// The default will be set regardless, but `get` calls with this ID
    /// will return `None` until a provider with this ID is registered.
    pub fn set_default(&mut self, id: ProviderId) {
        self.default_provider = Some(id);
    }

    /// Get list of available (healthy) providers
    ///
    /// This method checks the health status of each registered provider
    /// and returns only those that are currently available.
    ///
    /// # Note
    ///
    /// This is an async method because it calls `health_check()` on each
    /// provider, which may perform I/O operations.
    pub async fn available_providers(&self) -> Vec<ProviderId> {
        let mut available: Vec<ProviderId> = Vec::new();

        for (id, provider) in &self.providers {
            let status: ProviderStatus = provider.health_check().await;
            if status.is_available() {
                available.push(id.clone());
            }
        }

        available
    }

    /// Check if the registry contains a provider with the given ID
    pub fn contains(&self, id: &ProviderId) -> bool {
        self.providers.contains_key(id)
    }

    /// Get the number of registered providers
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// Check if the registry is empty
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

impl std::fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRegistry")
            .field("providers", &self.providers.keys().collect::<Vec<_>>())
            .field("default_provider", &self.default_provider)
            .finish()
    }
}

/// Register all enabled TTS providers based on feature flags and configuration.
///
/// This function creates a `ProviderRegistry` and populates it with all TTS providers
/// that are:
/// 1. Enabled via Cargo feature flags (e.g., `kokoro`, `openai-tts`, `avspeech`)
/// 2. Configured in the provided `Config`
///
/// Providers that fail to initialize are logged as warnings and skipped, allowing
/// the server to start with a subset of available providers.
///
/// # Arguments
///
/// * `config` - Server configuration containing provider settings
///
/// # Returns
///
/// A `ProviderRegistry` containing all successfully initialized providers.
/// If no providers could be initialized, an empty registry is returned.
///
/// # Example
///
/// ```ignore
/// let config = Config::load_default()?;
/// let registry = register_providers(&config).await;
///
/// if registry.is_empty() {
///     tracing::warn!("No TTS providers available");
/// }
/// ```
// Allow unused_async when no provider features are enabled, as the function
// becomes non-async in that case but we want a consistent API.
#[allow(clippy::unused_async)]
pub async fn register_providers(config: &yappy_core::Config) -> ProviderRegistry {
    use tracing::{debug, info, warn};

    let mut registry = ProviderRegistry::new();

    info!("Registering TTS providers based on feature flags");

    // Register Kokoro provider if feature is enabled
    #[cfg(feature = "kokoro")]
    {
        debug!("Kokoro feature enabled, attempting to register provider");
        match register_kokoro_provider(config).await {
            Ok(provider) => {
                info!("Kokoro provider registered successfully");
                registry.register(provider);
            }
            Err(e) => {
                warn!(error = %e, "Failed to initialize Kokoro provider, skipping");
            }
        }
    }

    #[cfg(not(feature = "kokoro"))]
    {
        debug!("Kokoro feature not enabled");
    }

    // Register OpenAI provider if feature is enabled
    // TODO: Implement when yappy-provider-openai crate exists
    #[cfg(feature = "openai-tts")]
    {
        debug!("OpenAI TTS feature enabled, but provider not yet implemented");
        // When implemented:
        // match register_openai_provider(config).await {
        //     Ok(provider) => {
        //         info!("OpenAI provider registered successfully");
        //         registry.register(provider);
        //     }
        //     Err(e) => {
        //         warn!(error = %e, "Failed to initialize OpenAI provider, skipping");
        //     }
        // }
    }

    // Register AVSpeech provider if feature is enabled (macOS only)
    // TODO: Implement when yappy-provider-avspeech crate exists
    #[cfg(feature = "avspeech")]
    {
        debug!("AVSpeech feature enabled, but provider not yet implemented");
        // When implemented:
        // match register_avspeech_provider(config).await {
        //     Ok(provider) => {
        //         info!("AVSpeech provider registered successfully");
        //         registry.register(provider);
        //     }
        //     Err(e) => {
        //         warn!(error = %e, "Failed to initialize AVSpeech provider, skipping");
        //     }
        // }
    }

    // Set default provider from config
    let default_id = ProviderId::new(&config.providers.default);
    if registry.contains(&default_id) {
        registry.set_default(default_id.clone());
        info!(default = %default_id.0, "Default provider set");
    } else if !registry.is_empty() {
        // Fall back to first available provider
        if let Some(first) = registry.list().first() {
            let fallback_id = first.id.clone();
            warn!(
                requested = %config.providers.default,
                fallback = %fallback_id.0,
                "Requested default provider not available, using fallback"
            );
            registry.set_default(fallback_id);
        }
    } else {
        warn!("No TTS providers registered, server will have limited functionality");
    }

    info!(
        num_providers = registry.len(),
        default = ?registry.default_provider().map(|p| p.0),
        "Provider registration complete"
    );

    registry
}

/// Register the Kokoro ONNX TTS provider.
///
/// Creates and initializes a Kokoro provider based on the configuration.
/// The provider will download the model from `HuggingFace` on first use if
/// `model_path` is set to "auto" or not specified.
#[cfg(feature = "kokoro")]
async fn register_kokoro_provider(
    config: &yappy_core::Config,
) -> Result<yappy_provider_kokoro::KokoroProvider, yappy_core::error::ProviderError> {
    use std::path::PathBuf;
    use tracing::debug;
    use yappy_provider_kokoro::{KokoroConfig, KokoroProvider};

    // Build Kokoro configuration from server config
    let kokoro_config = config.providers.kokoro.as_ref().map_or_else(
        || {
            debug!("Using default Kokoro configuration");
            KokoroConfig::default()
        },
        |cfg| {
            let model_path = if cfg.model_path == "auto" {
                None
            } else {
                Some(PathBuf::from(&cfg.model_path))
            };

            let voices_path = if cfg.voices_path == "auto" {
                None
            } else {
                Some(PathBuf::from(&cfg.voices_path))
            };

            debug!(
                model_path = ?model_path,
                voices_path = ?voices_path,
                "Using custom Kokoro configuration"
            );

            KokoroConfig {
                model_path,
                voices_path,
                default_voice: "af_bella".to_string(),
            }
        },
    );

    // Create and initialize the provider
    let provider = KokoroProvider::new(kokoro_config);

    // Load the model (this may download from HuggingFace)
    debug!("Loading Kokoro model (this may download on first run)");
    provider.load_model().await?;

    Ok(provider)
}

/// Application state shared across Axum handlers
///
/// This struct holds all the shared state needed by the server:
/// - Provider registry for TTS synthesis
/// - Configuration settings
/// - Server start time for uptime tracking
///
/// `AppState` is designed to be used with Axum's `State` extractor.
/// It implements `Clone` cheaply via `Arc` for all its fields.
///
/// # Example
///
/// ```ignore
/// let registry = ProviderRegistry::new();
/// let config = Config::load_default()?;
/// let state = AppState::new(config, registry);
///
/// // In an Axum handler:
/// async fn health(State(state): State<AppState>) -> impl IntoResponse {
///     let uptime = state.uptime();
///     Json(json!({ "uptime_secs": uptime.as_secs() }))
/// }
/// ```
#[derive(Clone)]
pub struct AppState {
    /// Thread-safe provider registry
    providers: Arc<ProviderRegistry>,
    /// Shared configuration
    config: Arc<Config>,
    /// Server start time for uptime calculation
    start_time: Instant,
}

impl AppState {
    /// Create new application state
    ///
    /// # Arguments
    ///
    /// * `config` - Server configuration
    /// * `providers` - Provider registry with registered TTS providers
    pub fn new(config: Config, providers: ProviderRegistry) -> Self {
        Self {
            providers: Arc::new(providers),
            config: Arc::new(config),
            start_time: Instant::now(),
        }
    }

    /// Get the server uptime
    pub fn uptime(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Get a reference to the provider registry
    pub fn providers(&self) -> &ProviderRegistry {
        &self.providers
    }

    /// Get a reference to the configuration
    pub fn config(&self) -> &Config {
        &self.config
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("providers", &self.providers)
            .field("config", &"<Config>")
            .field("start_time", &self.start_time)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use tokio_util::sync::CancellationToken;
    use yappy_core::{
        audio::{AudioFormat, AudioStream},
        error::ProviderError,
        session::VoiceConfig,
    };

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
            _cancel: CancellationToken,
        ) -> Result<AudioStream, ProviderError> {
            unimplemented!("Mock provider does not synthesize")
        }
    }

    #[test]
    fn test_registry_new() {
        let registry = ProviderRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(registry.default_provider().is_none());
    }

    #[test]
    fn test_registry_register() {
        let mut registry = ProviderRegistry::new();
        let provider = MockProvider::new("test", "Test Provider");

        registry.register(provider);

        assert!(!registry.is_empty());
        assert_eq!(registry.len(), 1);
        assert!(registry.contains(&ProviderId::new("test")));
    }

    #[test]
    fn test_registry_get() {
        let mut registry = ProviderRegistry::new();
        registry.register(MockProvider::new("kokoro", "Kokoro"));

        let provider = registry.get(&ProviderId::new("kokoro"));
        assert!(provider.is_some());
        assert_eq!(provider.unwrap().metadata().name, "Kokoro");

        let missing = registry.get(&ProviderId::new("nonexistent"));
        assert!(missing.is_none());
    }

    #[test]
    fn test_registry_list() {
        let mut registry = ProviderRegistry::new();
        registry.register(MockProvider::new("a", "Provider A"));
        registry.register(MockProvider::new("b", "Provider B"));

        let list = registry.list();
        assert_eq!(list.len(), 2);

        let ids: Vec<_> = list.iter().map(|m| m.id.0.as_str()).collect();
        assert!(ids.contains(&"a"));
        assert!(ids.contains(&"b"));
    }

    #[test]
    fn test_registry_default_provider() {
        let mut registry = ProviderRegistry::new();
        registry.register(MockProvider::new("kokoro", "Kokoro"));
        registry.register(MockProvider::new("openai", "OpenAI"));

        assert!(registry.default_provider().is_none());

        registry.set_default(ProviderId::new("kokoro"));
        assert_eq!(registry.default_provider(), Some(ProviderId::new("kokoro")));

        registry.set_default(ProviderId::new("openai"));
        assert_eq!(registry.default_provider(), Some(ProviderId::new("openai")));
    }

    #[tokio::test]
    async fn test_registry_available_providers() {
        let mut registry = ProviderRegistry::new();
        registry.register(MockProvider::new("available1", "Available 1"));
        registry.register(MockProvider::new("available2", "Available 2"));
        registry.register(MockProvider::unavailable(
            "unavailable",
            "Unavailable",
            "Not configured",
        ));

        let available = registry.available_providers().await;
        assert_eq!(available.len(), 2);

        let ids: Vec<_> = available.iter().map(|id| id.0.as_str()).collect();
        assert!(ids.contains(&"available1"));
        assert!(ids.contains(&"available2"));
        assert!(!ids.contains(&"unavailable"));
    }

    #[test]
    fn test_app_state_uptime() {
        let config = create_test_config();
        let registry = ProviderRegistry::new();
        let state = AppState::new(config, registry);

        // Uptime should be very small right after creation
        let uptime = state.uptime();
        assert!(uptime.as_millis() < 100);
    }

    #[test]
    fn test_app_state_providers() {
        let config = create_test_config();
        let mut registry = ProviderRegistry::new();
        registry.register(MockProvider::new("test", "Test"));

        let state = AppState::new(config, registry);

        assert_eq!(state.providers().len(), 1);
        assert!(state.providers().contains(&ProviderId::new("test")));
    }

    #[test]
    fn test_app_state_config() {
        let config = create_test_config();
        let registry = ProviderRegistry::new();
        let state = AppState::new(config, registry);

        assert_eq!(state.config().server.port, 3000);
    }

    #[test]
    fn test_app_state_clone() {
        let config = create_test_config();
        let registry = ProviderRegistry::new();
        let state = AppState::new(config, registry);

        let cloned = state.clone();

        // Both should share the same data via Arc
        assert_eq!(state.config().server.port, cloned.config().server.port);
    }

    /// Create a minimal test configuration
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
}
