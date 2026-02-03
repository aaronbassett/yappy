//! Application state and provider registry
//!
//! This module provides the shared state for the Yappy TTS server:
//! - [`ProviderRegistry`] - Thread-safe registry of TTS providers
//! - [`AppState`] - Application state shared across Axum handlers
//! - [`SynthesisPermit`] - RAII guard for per-provider synthesis concurrency limiting

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{debug, trace};
use yappy_core::provider::ProviderId;
use yappy_core::{Config, ProviderMetadata, ProviderStatus, TtsProvider};

use crate::shutdown::ShutdownCoordinator;

/// Registry holding all available TTS providers
///
/// The registry maps provider IDs to their implementations and tracks
/// which provider is the default. It also tracks the initialization status
/// of all compiled-in providers, including those that failed to initialize.
///
/// The `provider_statuses` field tracks the initialization state of all
/// known providers, while `providers` only contains successfully initialized
/// providers. This allows endpoints like `/health` and `/providers` to report
/// on all compiled-in providers.
///
/// # Example
///
/// ```ignore
/// let mut registry = ProviderRegistry::new();
/// registry.register(kokoro_provider);
/// registry.record_status(ProviderId::new("kokoro"), ProviderStatus::Available);
/// registry.record_status(
///     ProviderId::new("openai"),
///     ProviderStatus::NotConfigured { reason: "API key not set".to_string() }
/// );
/// registry.set_default(ProviderId::new("kokoro"));
///
/// // Get a provider
/// if let Some(provider) = registry.get(&ProviderId::new("kokoro")) {
///     let metadata = provider.metadata();
///     println!("Using provider: {}", metadata.name);
/// }
///
/// // Check status of all providers (including those not initialized)
/// for (id, status) in registry.all_statuses() {
///     println!("{}: {:?}", id, status);
/// }
/// ```
pub struct ProviderRegistry {
    /// Map of provider ID to provider implementation (only successfully initialized providers)
    providers: HashMap<ProviderId, Arc<dyn TtsProvider + Send + Sync>>,
    /// The default provider ID
    default_provider: Option<ProviderId>,
    /// Initialization status of all compiled-in providers
    ///
    /// This includes providers that failed to initialize, allowing the server
    /// to report on why certain providers are not available.
    provider_statuses: HashMap<ProviderId, ProviderStatus>,
    /// Per-provider semaphores for limiting concurrent synthesis operations (FR-019)
    ///
    /// Each provider has its own semaphore to prevent resource exhaustion.
    /// The semaphore capacity is configured via `max_concurrent_synthesis`.
    synthesis_semaphores: HashMap<ProviderId, Arc<Semaphore>>,
    /// Configured maximum concurrent synthesis operations per provider
    max_concurrent_synthesis: usize,
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self {
            providers: HashMap::new(),
            default_provider: None,
            provider_statuses: HashMap::new(),
            synthesis_semaphores: HashMap::new(),
            max_concurrent_synthesis: 4, // Default value
        }
    }
}

impl ProviderRegistry {
    /// Create an empty provider registry with default concurrency limit
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a provider registry with a custom concurrency limit
    ///
    /// # Arguments
    ///
    /// * `max_concurrent_synthesis` - Maximum concurrent synthesis operations per provider
    pub fn with_max_concurrent_synthesis(max_concurrent_synthesis: usize) -> Self {
        Self {
            providers: HashMap::new(),
            default_provider: None,
            provider_statuses: HashMap::new(),
            synthesis_semaphores: HashMap::new(),
            max_concurrent_synthesis,
        }
    }

    /// Register a TTS provider
    ///
    /// The provider's ID is extracted from its metadata. If a provider
    /// with the same ID already exists, it will be replaced. A semaphore
    /// is created for the provider to limit concurrent synthesis operations.
    ///
    /// # Arguments
    ///
    /// * `provider` - The TTS provider to register
    pub fn register(&mut self, provider: impl TtsProvider + 'static) {
        let metadata = provider.metadata();
        let id = metadata.id;

        // Create semaphore for this provider
        let semaphore = Arc::new(Semaphore::new(self.max_concurrent_synthesis));
        self.synthesis_semaphores.insert(id.clone(), semaphore);

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

    /// Record the initialization status of a provider
    ///
    /// This records whether a compiled-in provider initialized successfully,
    /// failed due to missing configuration, or is unavailable for other reasons.
    /// This status is separate from the dynamic health check status.
    ///
    /// # Arguments
    ///
    /// * `id` - The provider ID
    /// * `status` - The initialization status to record
    ///
    /// # Example
    ///
    /// ```ignore
    /// registry.record_status(
    ///     ProviderId::new("openai"),
    ///     ProviderStatus::NotConfigured { reason: "API key not set".to_string() }
    /// );
    /// ```
    pub fn record_status(&mut self, id: ProviderId, status: ProviderStatus) {
        self.provider_statuses.insert(id, status);
    }

    /// Get the initialization status of a provider
    ///
    /// Returns `None` if the provider ID is not known to the registry.
    ///
    /// # Arguments
    ///
    /// * `id` - The provider ID to look up
    pub fn get_status(&self, id: &ProviderId) -> Option<ProviderStatus> {
        self.provider_statuses.get(id).cloned()
    }

    /// Get all provider initialization statuses
    ///
    /// Returns a reference to the map of all compiled-in providers and their
    /// initialization statuses. This includes providers that:
    /// - Initialized successfully (`Available`)
    /// - Failed due to missing configuration (`NotConfigured`)
    /// - Are unavailable for other reasons (`Unavailable`)
    pub const fn all_statuses(&self) -> &HashMap<ProviderId, ProviderStatus> {
        &self.provider_statuses
    }

    /// Get the configured max concurrent synthesis limit
    pub const fn max_concurrent_synthesis(&self) -> usize {
        self.max_concurrent_synthesis
    }

    /// Acquire a synthesis permit for a provider (FR-019)
    ///
    /// This method implements per-provider concurrency limiting by acquiring
    /// a permit from the provider's semaphore. If no permits are available,
    /// the call will wait (async) until one becomes free.
    ///
    /// The returned `SynthesisPermit` is an RAII guard that automatically
    /// releases the permit when dropped.
    ///
    /// # Arguments
    ///
    /// * `provider_id` - The ID of the provider to acquire a permit for
    ///
    /// # Returns
    ///
    /// Returns `Some(SynthesisPermit)` if the provider exists and a permit
    /// was successfully acquired, `None` if the provider is not registered.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let permit = registry.acquire_synthesis_permit(&provider_id).await;
    /// if let Some(_permit) = permit {
    ///     // Perform synthesis - permit is held
    ///     let result = provider.synthesize(...).await;
    ///     // permit is automatically released when dropped
    /// }
    /// ```
    pub async fn acquire_synthesis_permit(
        &self,
        provider_id: &ProviderId,
    ) -> Option<SynthesisPermit> {
        let semaphore = self.synthesis_semaphores.get(provider_id)?.clone();

        // Log when we're about to wait for a permit
        let available = semaphore.available_permits();
        if available == 0 {
            debug!(
                provider = %provider_id.0,
                max_concurrent = self.max_concurrent_synthesis,
                "Synthesis queued - waiting for permit (all slots in use)"
            );
        } else {
            trace!(
                provider = %provider_id.0,
                available_permits = available,
                "Acquiring synthesis permit"
            );
        }

        let start = Instant::now();

        // Acquire permit (this will wait if none available)
        let permit = semaphore.clone().acquire_owned().await.ok()?;

        let wait_time = start.elapsed();
        if wait_time.as_millis() > 0 {
            debug!(
                provider = %provider_id.0,
                wait_ms = wait_time.as_millis(),
                "Synthesis permit acquired after waiting"
            );
        } else {
            trace!(
                provider = %provider_id.0,
                "Synthesis permit acquired immediately"
            );
        }

        Some(SynthesisPermit {
            _permit: permit,
            provider_id: provider_id.clone(),
        })
    }

    /// Try to acquire a synthesis permit without waiting
    ///
    /// This is a non-blocking version of `acquire_synthesis_permit` that
    /// returns immediately if no permit is available.
    ///
    /// # Arguments
    ///
    /// * `provider_id` - The ID of the provider to acquire a permit for
    ///
    /// # Returns
    ///
    /// Returns `Some(SynthesisPermit)` if a permit was immediately available,
    /// `None` if the provider doesn't exist or no permits are available.
    pub fn try_acquire_synthesis_permit(
        &self,
        provider_id: &ProviderId,
    ) -> Option<SynthesisPermit> {
        let semaphore = self.synthesis_semaphores.get(provider_id)?.clone();

        let permit = semaphore.try_acquire_owned().ok()?;

        trace!(
            provider = %provider_id.0,
            "Synthesis permit acquired (try)"
        );

        Some(SynthesisPermit {
            _permit: permit,
            provider_id: provider_id.clone(),
        })
    }
}

/// RAII guard for a synthesis permit (FR-019)
///
/// This guard is returned by `ProviderRegistry::acquire_synthesis_permit` and
/// automatically releases the synthesis slot when dropped. This ensures that
/// concurrent synthesis operations are properly limited even if the synthesis
/// task panics or is cancelled.
///
/// # Example
///
/// ```ignore
/// // Permit is acquired
/// let permit = registry.acquire_synthesis_permit(&provider_id).await?;
///
/// // Do synthesis work...
/// let result = provider.synthesize(...).await;
///
/// // Permit is automatically released when `permit` goes out of scope
/// ```
#[derive(Debug)]
pub struct SynthesisPermit {
    /// The underlying semaphore permit (released on drop)
    _permit: OwnedSemaphorePermit,
    /// The provider ID this permit is for (for logging)
    provider_id: ProviderId,
}

impl Drop for SynthesisPermit {
    fn drop(&mut self) {
        trace!(
            provider = %self.provider_id.0,
            "Synthesis permit released"
        );
    }
}

impl std::fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRegistry")
            .field("providers", &self.providers.keys().collect::<Vec<_>>())
            .field("default_provider", &self.default_provider)
            .field("provider_statuses", &self.provider_statuses)
            .field("max_concurrent_synthesis", &self.max_concurrent_synthesis)
            .field(
                "synthesis_semaphores",
                &self.synthesis_semaphores.keys().collect::<Vec<_>>(),
            )
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
#[allow(clippy::unused_async, clippy::too_many_lines)]
pub async fn register_providers(config: &yappy_core::Config) -> ProviderRegistry {
    use tracing::{debug, info, warn};

    let max_concurrent = config.providers.max_concurrent_synthesis;
    let mut registry = ProviderRegistry::with_max_concurrent_synthesis(max_concurrent);

    info!(
        max_concurrent_synthesis = max_concurrent,
        "Registering TTS providers based on feature flags"
    );

    // Register Kokoro provider if feature is enabled
    #[cfg(feature = "kokoro")]
    {
        let kokoro_id = ProviderId::new("kokoro");
        debug!("Kokoro feature enabled, attempting to register provider");
        match register_kokoro_provider(config).await {
            Ok(provider) => {
                info!("Kokoro provider registered successfully");
                registry.register(provider);
                registry.record_status(kokoro_id, ProviderStatus::Available);
            }
            Err(e) => {
                warn!(error = %e, "Failed to initialize Kokoro provider, skipping");
                registry.record_status(
                    kokoro_id,
                    ProviderStatus::Unavailable {
                        reason: e.to_string(),
                    },
                );
            }
        }
    }

    #[cfg(not(feature = "kokoro"))]
    {
        debug!("Kokoro feature not enabled");
    }

    // Register OpenAI provider if feature is enabled
    #[cfg(feature = "openai-tts")]
    {
        let openai_id = ProviderId::new("openai");
        debug!("OpenAI TTS feature enabled, attempting to register provider");
        match register_openai_provider(config).await {
            Ok(provider) => {
                info!("OpenAI provider registered successfully");
                registry.register(provider);
                registry.record_status(openai_id, ProviderStatus::Available);
            }
            Err(e) => {
                warn!(error = %e, "Failed to initialize OpenAI provider, skipping");
                registry.record_status(
                    openai_id,
                    ProviderStatus::NotConfigured {
                        reason: e.to_string(),
                    },
                );
            }
        }
    }

    // Register AVSpeech provider if feature is enabled (macOS only)
    #[cfg(feature = "avspeech")]
    {
        let avspeech_id = ProviderId::new("avspeech");

        // AVSpeech is macOS only - check platform
        #[cfg(target_os = "macos")]
        {
            debug!("AVSpeech feature enabled on macOS, attempting to register provider");
            match register_avspeech_provider(config).await {
                Ok(provider) => {
                    info!("AVSpeech provider registered successfully");
                    registry.register(provider);
                    registry.record_status(avspeech_id, ProviderStatus::Available);
                }
                Err(e) => {
                    warn!(error = %e, "Failed to initialize AVSpeech provider, skipping");
                    registry.record_status(
                        avspeech_id,
                        ProviderStatus::Unavailable {
                            reason: e.to_string(),
                        },
                    );
                }
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            debug!("AVSpeech feature enabled but not on macOS, marking unavailable");
            registry.record_status(
                avspeech_id,
                ProviderStatus::Unavailable {
                    reason: "AVSpeech is only available on macOS".to_string(),
                },
            );
        }
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

/// Register the `OpenAI` TTS provider.
///
/// Creates an `OpenAI` provider based on the configuration.
/// Requires an API key to be configured (via config or `OPENAI_API_KEY` env var).
#[cfg(feature = "openai-tts")]
async fn register_openai_provider(
    config: &yappy_core::Config,
) -> Result<yappy_provider_openai::OpenAiProvider, yappy_core::error::ProviderError> {
    use tracing::debug;
    use yappy_provider_openai::{OpenAiConfig, OpenAiProvider};

    // Get OpenAI configuration
    let openai_config = config.providers.openai.as_ref().map_or_else(
        || {
            // Check for API key in environment variable
            std::env::var("OPENAI_API_KEY").map_or_else(
                |_| {
                    debug!("No OpenAI configuration found");
                    OpenAiConfig::default()
                },
                |api_key| {
                    debug!("Using OpenAI API key from environment variable");
                    OpenAiConfig::new(api_key)
                },
            )
        },
        |cfg| {
            // Expand environment variable references in API key (e.g., "$OPENAI_API_KEY")
            let api_key = if cfg.api_key.starts_with('$') {
                let var_name = &cfg.api_key[1..];
                std::env::var(var_name).unwrap_or_default()
            } else {
                cfg.api_key.clone()
            };

            debug!(
                model = %cfg.model,
                "Using custom OpenAI configuration"
            );

            OpenAiConfig::with_model(api_key, cfg.model.clone())
        },
    );

    // Validate that API key is configured
    if !openai_config.is_api_key_configured() {
        return Err(yappy_core::error::ProviderError::NotConfigured {
            reason: "OpenAI API key not configured. Set OPENAI_API_KEY environment variable or configure in yappy.toml".to_string(),
        });
    }

    let provider = OpenAiProvider::new(openai_config);

    // Health check to validate configuration
    let status = provider.health_check().await;
    if !status.is_available() {
        return Err(yappy_core::error::ProviderError::NotConfigured {
            reason: format!("OpenAI provider health check failed: {status:?}"),
        });
    }

    Ok(provider)
}

/// Register the AVSpeech provider (macOS only).
///
/// Creates an AVSpeech provider using the system's AVSpeechSynthesizer.
/// Only available on macOS targets.
#[cfg(all(feature = "avspeech", target_os = "macos"))]
async fn register_avspeech_provider(
    config: &yappy_core::Config,
) -> Result<yappy_provider_avspeech::AvSpeechProvider, yappy_core::error::ProviderError> {
    use tracing::debug;
    use yappy_provider_avspeech::{AvSpeechConfig, AvSpeechProvider};

    // Check if AVSpeech is enabled in config
    let enabled = config
        .providers
        .avspeech
        .as_ref()
        .map_or(true, |cfg| cfg.enabled);

    if !enabled {
        return Err(yappy_core::error::ProviderError::NotConfigured {
            reason: "AVSpeech is disabled in configuration".to_string(),
        });
    }

    debug!("Using default AVSpeech configuration");
    let avspeech_config = AvSpeechConfig::default();
    let provider = AvSpeechProvider::new(avspeech_config);

    Ok(provider)
}

/// Application state shared across Axum handlers
///
/// This struct holds all the shared state needed by the server:
/// - Provider registry for TTS synthesis
/// - Configuration settings
/// - Server start time for uptime tracking
/// - Shutdown coordinator for graceful shutdown
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
    /// Shutdown coordinator for graceful shutdown (FR-027, SC-007)
    shutdown: Arc<ShutdownCoordinator>,
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
            shutdown: Arc::new(ShutdownCoordinator::new()),
        }
    }

    /// Create new application state with a custom shutdown coordinator
    ///
    /// This is useful for testing or when you need to share a shutdown
    /// coordinator across multiple components.
    ///
    /// # Arguments
    ///
    /// * `config` - Server configuration
    /// * `providers` - Provider registry with registered TTS providers
    /// * `shutdown` - Shutdown coordinator for graceful shutdown
    pub fn with_shutdown_coordinator(
        config: Config,
        providers: ProviderRegistry,
        shutdown: Arc<ShutdownCoordinator>,
    ) -> Self {
        Self {
            providers: Arc::new(providers),
            config: Arc::new(config),
            start_time: Instant::now(),
            shutdown,
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

    /// Get the shutdown coordinator
    ///
    /// Used by WebSocket handlers to register sessions and check for
    /// shutdown signals.
    pub const fn shutdown(&self) -> &Arc<ShutdownCoordinator> {
        &self.shutdown
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("providers", &self.providers)
            .field("config", &"<Config>")
            .field("start_time", &self.start_time)
            .field("shutdown", &self.shutdown)
            .finish()
    }
}

#[cfg(test)]
#[allow(clippy::significant_drop_tightening)]
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
        assert!(registry.all_statuses().is_empty());
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
    fn test_registry_record_status() {
        let mut registry = ProviderRegistry::new();

        // Record an Available status
        registry.record_status(ProviderId::new("kokoro"), ProviderStatus::Available);

        // Record a NotConfigured status
        registry.record_status(
            ProviderId::new("openai"),
            ProviderStatus::NotConfigured {
                reason: "API key not set".to_string(),
            },
        );

        // Record an Unavailable status
        registry.record_status(
            ProviderId::new("avspeech"),
            ProviderStatus::Unavailable {
                reason: "macOS only".to_string(),
            },
        );

        // Verify all statuses are recorded
        assert_eq!(registry.all_statuses().len(), 3);
    }

    #[test]
    fn test_registry_get_status() {
        let mut registry = ProviderRegistry::new();

        // Record statuses
        registry.record_status(ProviderId::new("kokoro"), ProviderStatus::Available);
        registry.record_status(
            ProviderId::new("openai"),
            ProviderStatus::NotConfigured {
                reason: "API key not set".to_string(),
            },
        );

        // Get existing status
        let kokoro_status = registry.get_status(&ProviderId::new("kokoro"));
        assert!(kokoro_status.is_some());
        assert!(kokoro_status.unwrap().is_available());

        // Get another existing status
        let openai_status = registry.get_status(&ProviderId::new("openai"));
        assert!(openai_status.is_some());
        match openai_status.unwrap() {
            ProviderStatus::NotConfigured { reason } => {
                assert_eq!(reason, "API key not set");
            }
            _ => panic!("Expected NotConfigured status"),
        }

        // Get non-existent status
        let missing = registry.get_status(&ProviderId::new("nonexistent"));
        assert!(missing.is_none());
    }

    #[test]
    fn test_registry_all_statuses() {
        let mut registry = ProviderRegistry::new();

        // Initially empty
        assert!(registry.all_statuses().is_empty());

        // Add statuses
        registry.record_status(ProviderId::new("kokoro"), ProviderStatus::Available);
        registry.record_status(
            ProviderId::new("openai"),
            ProviderStatus::NotConfigured {
                reason: "API key not set".to_string(),
            },
        );
        registry.record_status(
            ProviderId::new("avspeech"),
            ProviderStatus::Unavailable {
                reason: "macOS only".to_string(),
            },
        );

        // Verify all_statuses returns the correct map
        let statuses = registry.all_statuses();
        assert_eq!(statuses.len(), 3);
        assert!(statuses.contains_key(&ProviderId::new("kokoro")));
        assert!(statuses.contains_key(&ProviderId::new("openai")));
        assert!(statuses.contains_key(&ProviderId::new("avspeech")));

        // Verify individual status types
        assert!(statuses
            .get(&ProviderId::new("kokoro"))
            .unwrap()
            .is_available());
        assert!(!statuses
            .get(&ProviderId::new("openai"))
            .unwrap()
            .is_available());
        assert!(!statuses
            .get(&ProviderId::new("avspeech"))
            .unwrap()
            .is_available());
    }

    #[test]
    fn test_registry_status_overwrite() {
        let mut registry = ProviderRegistry::new();

        // Record initial status
        registry.record_status(
            ProviderId::new("kokoro"),
            ProviderStatus::NotConfigured {
                reason: "Model not found".to_string(),
            },
        );

        // Verify initial status
        let status = registry.get_status(&ProviderId::new("kokoro")).unwrap();
        assert!(!status.is_available());

        // Overwrite with new status
        registry.record_status(ProviderId::new("kokoro"), ProviderStatus::Available);

        // Verify status was updated
        let status = registry.get_status(&ProviderId::new("kokoro")).unwrap();
        assert!(status.is_available());

        // Only one entry should exist
        assert_eq!(registry.all_statuses().len(), 1);
    }

    #[test]
    fn test_registry_statuses_independent_of_providers() {
        let mut registry = ProviderRegistry::new();

        // Register an actual provider
        registry.register(MockProvider::new("kokoro", "Kokoro"));
        registry.record_status(ProviderId::new("kokoro"), ProviderStatus::Available);

        // Record status for provider that's NOT registered
        registry.record_status(
            ProviderId::new("openai"),
            ProviderStatus::NotConfigured {
                reason: "API key not set".to_string(),
            },
        );

        // providers map has 1 entry
        assert_eq!(registry.len(), 1);
        assert!(registry.contains(&ProviderId::new("kokoro")));
        assert!(!registry.contains(&ProviderId::new("openai")));

        // provider_statuses has 2 entries
        assert_eq!(registry.all_statuses().len(), 2);
        assert!(registry.get_status(&ProviderId::new("kokoro")).is_some());
        assert!(registry.get_status(&ProviderId::new("openai")).is_some());
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
                max_concurrent_synthesis: 4,
            },
            buffer: yappy_core::config::BufferConfigToml::default(),
        }
    }

    // ========== Synthesis Permit Tests (FR-019) ==========

    #[test]
    fn test_registry_creates_semaphore_on_register() {
        let mut registry = ProviderRegistry::with_max_concurrent_synthesis(8);
        let provider = MockProvider::new("test", "Test Provider");

        registry.register(provider);

        // Verify semaphore was created
        assert!(registry
            .synthesis_semaphores
            .contains_key(&ProviderId::new("test")));
    }

    #[test]
    fn test_registry_max_concurrent_synthesis() {
        let registry = ProviderRegistry::with_max_concurrent_synthesis(16);
        assert_eq!(registry.max_concurrent_synthesis(), 16);

        let default_registry = ProviderRegistry::new();
        assert_eq!(default_registry.max_concurrent_synthesis(), 4);
    }

    #[tokio::test]
    async fn test_acquire_synthesis_permit_success() {
        let mut registry = ProviderRegistry::with_max_concurrent_synthesis(2);
        registry.register(MockProvider::new("test", "Test Provider"));

        let provider_id = ProviderId::new("test");

        // Should successfully acquire first permit
        let permit1 = registry.acquire_synthesis_permit(&provider_id).await;
        assert!(permit1.is_some());

        // Should successfully acquire second permit
        let permit2 = registry.acquire_synthesis_permit(&provider_id).await;
        assert!(permit2.is_some());
    }

    #[tokio::test]
    async fn test_acquire_synthesis_permit_nonexistent_provider() {
        let registry = ProviderRegistry::new();
        let provider_id = ProviderId::new("nonexistent");

        let permit = registry.acquire_synthesis_permit(&provider_id).await;
        assert!(permit.is_none());
    }

    #[test]
    fn test_try_acquire_synthesis_permit() {
        let mut registry = ProviderRegistry::with_max_concurrent_synthesis(1);
        registry.register(MockProvider::new("test", "Test Provider"));

        let provider_id = ProviderId::new("test");

        // Should successfully acquire first permit
        let permit1 = registry.try_acquire_synthesis_permit(&provider_id);
        assert!(permit1.is_some());

        // Should fail to acquire second permit (no blocking)
        let permit2 = registry.try_acquire_synthesis_permit(&provider_id);
        assert!(permit2.is_none());

        // After dropping first permit, should succeed again
        drop(permit1);
        let permit3 = registry.try_acquire_synthesis_permit(&provider_id);
        assert!(permit3.is_some());
    }

    #[test]
    fn test_try_acquire_synthesis_permit_nonexistent_provider() {
        let registry = ProviderRegistry::new();
        let provider_id = ProviderId::new("nonexistent");

        let permit = registry.try_acquire_synthesis_permit(&provider_id);
        assert!(permit.is_none());
    }

    #[tokio::test]
    async fn test_synthesis_permit_release_on_drop() {
        let mut registry = ProviderRegistry::with_max_concurrent_synthesis(1);
        registry.register(MockProvider::new("test", "Test Provider"));

        let provider_id = ProviderId::new("test");
        let semaphore = registry
            .synthesis_semaphores
            .get(&provider_id)
            .unwrap()
            .clone();

        // Initial state: 1 permit available
        assert_eq!(semaphore.available_permits(), 1);

        {
            let _permit = registry
                .acquire_synthesis_permit(&provider_id)
                .await
                .unwrap();
            // While permit is held: 0 permits available
            assert_eq!(semaphore.available_permits(), 0);
        }

        // After permit is dropped: 1 permit available again
        assert_eq!(semaphore.available_permits(), 1);
    }

    #[tokio::test]
    async fn test_synthesis_permit_concurrent_limiting() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;
        use tokio::time::timeout;

        let registry = Arc::new({
            let mut r = ProviderRegistry::with_max_concurrent_synthesis(2);
            r.register(MockProvider::new("test", "Test Provider"));
            r
        });

        let provider_id = ProviderId::new("test");
        let active_count = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));

        // Spawn 5 tasks that each try to acquire a permit
        let mut handles = Vec::new();
        for _ in 0..5 {
            let registry = registry.clone();
            let provider_id = provider_id.clone();
            let active_count = active_count.clone();
            let max_concurrent = max_concurrent.clone();

            handles.push(tokio::spawn(async move {
                let _permit = registry
                    .acquire_synthesis_permit(&provider_id)
                    .await
                    .unwrap();

                // Increment active count
                let current = active_count.fetch_add(1, Ordering::SeqCst) + 1;

                // Track max concurrent
                max_concurrent.fetch_max(current, Ordering::SeqCst);

                // Simulate some work
                tokio::time::sleep(Duration::from_millis(10)).await;

                // Decrement active count
                active_count.fetch_sub(1, Ordering::SeqCst);
            }));
        }

        // Wait for all tasks with a timeout
        let result = timeout(Duration::from_secs(5), async {
            for handle in handles {
                handle.await.unwrap();
            }
        })
        .await;

        assert!(result.is_ok(), "Tasks should complete within timeout");

        // Max concurrent should never exceed 2
        assert!(
            max_concurrent.load(Ordering::SeqCst) <= 2,
            "Max concurrent syntheses should not exceed limit"
        );
    }
}
