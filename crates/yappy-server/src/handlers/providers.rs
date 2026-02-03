//! GET /providers endpoint handler
//!
//! Lists all compiled TTS providers with their capabilities.
//! Available providers include full metadata (voices, formats, etc.),
//! while unavailable providers only include status and reason.

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use yappy_core::audio::AudioFormat;
use yappy_core::provider::{ProviderStatus, VoiceInfo};

use crate::state::AppState;

/// Response from GET /providers endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvidersResponse {
    /// List of all providers with their status and capabilities
    pub providers: Vec<ProviderInfo>,

    /// ID of the default provider (if set)
    pub default_provider: Option<String>,
}

/// Provider information returned in the providers list.
///
/// This uses an untagged enum to produce different JSON shapes:
/// - Available providers include full metadata (voices, formats, `options_schema`)
/// - Unavailable providers only include id, name, description, status, and reason
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProviderInfo {
    /// Provider that is available with full metadata
    Available(AvailableProvider),

    /// Provider that is not available (`not_configured` or `unavailable`)
    Unavailable(UnavailableProvider),
}

/// Available provider with full metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailableProvider {
    /// Unique provider identifier
    pub id: String,

    /// Human-readable name
    pub name: String,

    /// Provider description
    pub description: String,

    /// Provider status (always "available")
    pub status: String,

    /// Available voices
    pub voices: Vec<VoiceInfo>,

    /// Supported audio formats
    pub supported_formats: Vec<SupportedFormat>,

    /// Provider-specific options schema (JSON Schema)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options_schema: Option<serde_json::Value>,
}

/// Unavailable provider with minimal information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnavailableProvider {
    /// Unique provider identifier
    pub id: String,

    /// Human-readable name
    pub name: String,

    /// Provider description
    pub description: String,

    /// Provider status (`not_configured` or `unavailable`)
    pub status: String,

    /// Human-readable reason why provider is not available
    pub reason: String,
}

/// Supported audio format as specified in the API contract
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupportedFormat {
    /// Codec name (e.g., "pcm", "opus", "mp3")
    pub codec: String,

    /// Supported sample rates in Hz
    pub sample_rates: Vec<u32>,
}

/// Converts a list of [`AudioFormat`]s to [`SupportedFormat`] entries.
///
/// Groups formats by codec and collects unique sample rates for each codec.
fn audio_formats_to_supported(formats: &[AudioFormat]) -> Vec<SupportedFormat> {
    use std::collections::HashMap;

    let mut codec_rates: HashMap<String, Vec<u32>> = HashMap::new();

    for format in formats {
        let codec_name = format!("{}", format.codec);
        let rates = codec_rates.entry(codec_name).or_default();
        if !rates.contains(&format.sample_rate) {
            rates.push(format.sample_rate);
        }
    }

    codec_rates
        .into_iter()
        .map(|(codec, mut sample_rates)| {
            sample_rates.sort_unstable();
            SupportedFormat {
                codec,
                sample_rates,
            }
        })
        .collect()
}

/// List all TTS providers with their capabilities
///
/// Returns metadata for all compiled-in TTS providers. Available providers
/// include full metadata (voices, formats, options schema), while unavailable
/// providers only include their status and reason.
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
///       "description": "Local ONNX-based TTS model with natural voices",
///       "status": "available",
///       "voices": [...],
///       "supported_formats": [...],
///       "options_schema": {...}
///     },
///     {
///       "id": "openai",
///       "name": "OpenAI TTS",
///       "description": "OpenAI's cloud-based text-to-speech API",
///       "status": "not_configured",
///       "reason": "API key not set"
///     }
///   ],
///   "default_provider": "kokoro"
/// }
/// ```
pub async fn list_providers(State(state): State<AppState>) -> Json<ProvidersResponse> {
    use crate::state::ProviderRegistry;

    /// Helper to get provider name and description from registry or defaults
    fn get_name_and_description(
        registry: &ProviderRegistry,
        provider_id: &yappy_core::provider::ProviderId,
    ) -> (String, String) {
        registry.get(provider_id).map_or_else(
            || {
                (
                    format_provider_name(&provider_id.0),
                    get_provider_description(&provider_id.0),
                )
            },
            |provider| {
                let metadata = provider.metadata();
                (metadata.name, metadata.description)
            },
        )
    }

    let registry = state.providers();
    let mut providers: Vec<ProviderInfo> = Vec::new();

    // Iterate over all known provider statuses (includes unavailable providers)
    for (provider_id, status) in registry.all_statuses() {
        let provider_info = match status {
            ProviderStatus::Available => {
                // For available providers, get full metadata from the actual provider
                registry.get(provider_id).map_or_else(
                    || {
                        // Status says available but provider not in registry - treat as unavailable
                        ProviderInfo::Unavailable(UnavailableProvider {
                            id: provider_id.0.clone(),
                            name: provider_id.0.clone(),
                            description: String::new(),
                            status: "unavailable".to_string(),
                            reason: "Provider registered but not found".to_string(),
                        })
                    },
                    |provider| {
                        let metadata = provider.metadata();
                        ProviderInfo::Available(AvailableProvider {
                            id: metadata.id.0,
                            name: metadata.name,
                            description: metadata.description,
                            status: "available".to_string(),
                            voices: metadata.voices,
                            supported_formats: audio_formats_to_supported(
                                &metadata.supported_formats,
                            ),
                            options_schema: metadata.options_schema,
                        })
                    },
                )
            }
            ProviderStatus::NotConfigured { reason } => {
                let (name, description) = get_name_and_description(registry, provider_id);
                ProviderInfo::Unavailable(UnavailableProvider {
                    id: provider_id.0.clone(),
                    name,
                    description,
                    status: "not_configured".to_string(),
                    reason: reason.clone(),
                })
            }
            ProviderStatus::Unavailable { reason } => {
                let (name, description) = get_name_and_description(registry, provider_id);
                ProviderInfo::Unavailable(UnavailableProvider {
                    id: provider_id.0.clone(),
                    name,
                    description,
                    status: "unavailable".to_string(),
                    reason: reason.clone(),
                })
            }
        };

        providers.push(provider_info);
    }

    // Sort providers by ID for consistent ordering
    providers.sort_by(|a, b| {
        let id_a = match a {
            ProviderInfo::Available(p) => &p.id,
            ProviderInfo::Unavailable(p) => &p.id,
        };
        let id_b = match b {
            ProviderInfo::Available(p) => &p.id,
            ProviderInfo::Unavailable(p) => &p.id,
        };
        id_a.cmp(id_b)
    });

    let default_provider = registry.default_provider().map(|id| id.0);

    Json(ProvidersResponse {
        providers,
        default_provider,
    })
}

/// Format a provider ID into a human-readable name
fn format_provider_name(id: &str) -> String {
    match id {
        "kokoro" => "Kokoro 82M".to_string(),
        "openai" => "OpenAI TTS".to_string(),
        "avspeech" => "AVSpeechSynthesizer".to_string(),
        other => {
            // Capitalize first letter
            let mut chars = other.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        }
    }
}

/// Get a default description for a provider by ID
fn get_provider_description(id: &str) -> String {
    match id {
        "kokoro" => "Local ONNX-based TTS model with natural voices".to_string(),
        "openai" => "OpenAI's cloud-based text-to-speech API".to_string(),
        "avspeech" => "macOS built-in speech synthesis".to_string(),
        _ => "TTS provider".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ProviderRegistry;
    use async_trait::async_trait;
    use tokio_util::sync::CancellationToken;
    use yappy_core::{
        audio::{AudioCodec, AudioStream},
        config::{BufferConfigToml, Config, ProvidersConfig, ServerConfig},
        error::ProviderError,
        provider::{ProviderId, ProviderMetadata},
        session::VoiceConfig,
        TtsProvider,
    };

    /// Mock TTS provider for testing
    struct MockProvider {
        id: String,
        name: String,
        description: String,
        voices: Vec<VoiceInfo>,
        formats: Vec<AudioFormat>,
        status: ProviderStatus,
    }

    impl MockProvider {
        fn new(id: &str, name: &str) -> Self {
            Self {
                id: id.to_string(),
                name: name.to_string(),
                description: format!("Mock {} provider", name),
                voices: vec![VoiceInfo {
                    id: "voice1".to_string(),
                    name: "Test Voice".to_string(),
                    language: "en-US".to_string(),
                    gender: Some(yappy_core::provider::VoiceGender::Female),
                    sample_url: None,
                }],
                formats: vec![
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
                status: ProviderStatus::Available,
            }
        }
    }

    #[async_trait]
    impl TtsProvider for MockProvider {
        fn metadata(&self) -> ProviderMetadata {
            ProviderMetadata {
                id: ProviderId::new(&self.id),
                name: self.name.clone(),
                description: self.description.clone(),
                voices: self.voices.clone(),
                supported_formats: self.formats.clone(),
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

    fn create_test_config() -> Config {
        Config {
            server: ServerConfig::default(),
            providers: ProvidersConfig {
                default: "test".to_string(),
                openai: None,
                kokoro: None,
                avspeech: None,
            },
            buffer: BufferConfigToml::default(),
        }
    }

    #[tokio::test]
    async fn test_list_providers_with_available_provider() {
        let config = create_test_config();
        let mut registry = ProviderRegistry::new();

        // Register an available provider
        let provider = MockProvider::new("test", "Test Provider");
        registry.register(provider);
        registry.record_status(ProviderId::new("test"), ProviderStatus::Available);
        registry.set_default(ProviderId::new("test"));

        let state = AppState::new(config, registry);

        let Json(response) = list_providers(State(state)).await;

        assert_eq!(response.providers.len(), 1);
        assert_eq!(response.default_provider, Some("test".to_string()));

        // Check that the provider is available with full metadata
        match &response.providers[0] {
            ProviderInfo::Available(p) => {
                assert_eq!(p.id, "test");
                assert_eq!(p.name, "Test Provider");
                assert_eq!(p.status, "available");
                assert!(!p.voices.is_empty());
                assert!(!p.supported_formats.is_empty());
            }
            ProviderInfo::Unavailable(_) => {
                panic!("Expected Available provider");
            }
        }
    }

    #[tokio::test]
    async fn test_list_providers_with_unavailable_provider() {
        let config = create_test_config();
        let mut registry = ProviderRegistry::new();

        // Record an unavailable provider status (no actual provider registered)
        registry.record_status(
            ProviderId::new("openai"),
            ProviderStatus::NotConfigured {
                reason: "API key not set".to_string(),
            },
        );

        let state = AppState::new(config, registry);

        let Json(response) = list_providers(State(state)).await;

        assert_eq!(response.providers.len(), 1);

        // Check that the provider is unavailable with minimal info
        match &response.providers[0] {
            ProviderInfo::Available(_) => {
                panic!("Expected Unavailable provider");
            }
            ProviderInfo::Unavailable(p) => {
                assert_eq!(p.id, "openai");
                assert_eq!(p.status, "not_configured");
                assert_eq!(p.reason, "API key not set");
            }
        }
    }

    #[tokio::test]
    async fn test_list_providers_mixed() {
        let config = create_test_config();
        let mut registry = ProviderRegistry::new();

        // Register an available provider
        let provider = MockProvider::new("kokoro", "Kokoro 82M");
        registry.register(provider);
        registry.record_status(ProviderId::new("kokoro"), ProviderStatus::Available);
        registry.set_default(ProviderId::new("kokoro"));

        // Record an unavailable provider
        registry.record_status(
            ProviderId::new("openai"),
            ProviderStatus::NotConfigured {
                reason: "API key not set".to_string(),
            },
        );

        // Record another unavailable provider
        registry.record_status(
            ProviderId::new("avspeech"),
            ProviderStatus::Unavailable {
                reason: "macOS only".to_string(),
            },
        );

        let state = AppState::new(config, registry);

        let Json(response) = list_providers(State(state)).await;

        // Should have all 3 providers
        assert_eq!(response.providers.len(), 3);
        assert_eq!(response.default_provider, Some("kokoro".to_string()));

        // Providers should be sorted by ID
        let ids: Vec<&str> = response
            .providers
            .iter()
            .map(|p| match p {
                ProviderInfo::Available(a) => a.id.as_str(),
                ProviderInfo::Unavailable(u) => u.id.as_str(),
            })
            .collect();
        assert_eq!(ids, vec!["avspeech", "kokoro", "openai"]);

        // Check each provider has correct status
        for provider in &response.providers {
            match provider {
                ProviderInfo::Available(p) => {
                    assert_eq!(p.id, "kokoro");
                    assert_eq!(p.status, "available");
                }
                ProviderInfo::Unavailable(p) => {
                    assert!(p.id == "openai" || p.id == "avspeech");
                    if p.id == "openai" {
                        assert_eq!(p.status, "not_configured");
                        assert_eq!(p.reason, "API key not set");
                    } else {
                        assert_eq!(p.status, "unavailable");
                        assert_eq!(p.reason, "macOS only");
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn test_list_providers_empty() {
        let config = create_test_config();
        let registry = ProviderRegistry::new();

        let state = AppState::new(config, registry);

        let Json(response) = list_providers(State(state)).await;

        assert!(response.providers.is_empty());
        assert!(response.default_provider.is_none());
    }

    #[test]
    fn test_audio_formats_to_supported() {
        let formats = vec![
            AudioFormat {
                codec: AudioCodec::Pcm,
                sample_rate: 24000,
                channels: 1,
                bits_per_sample: Some(16),
            },
            AudioFormat {
                codec: AudioCodec::Pcm,
                sample_rate: 48000,
                channels: 1,
                bits_per_sample: Some(16),
            },
            AudioFormat {
                codec: AudioCodec::Opus,
                sample_rate: 48000,
                channels: 1,
                bits_per_sample: None,
            },
        ];

        let supported = audio_formats_to_supported(&formats);

        // Should have 2 codec entries (pcm and opus)
        assert_eq!(supported.len(), 2);

        // Find each codec and verify sample rates
        for format in &supported {
            match format.codec.as_str() {
                "pcm" => {
                    assert_eq!(format.sample_rates, vec![24000, 48000]);
                }
                "opus" => {
                    assert_eq!(format.sample_rates, vec![48000]);
                }
                other => panic!("Unexpected codec: {}", other),
            }
        }
    }

    #[test]
    fn test_format_provider_name() {
        assert_eq!(format_provider_name("kokoro"), "Kokoro 82M");
        assert_eq!(format_provider_name("openai"), "OpenAI TTS");
        assert_eq!(format_provider_name("avspeech"), "AVSpeechSynthesizer");
        assert_eq!(format_provider_name("custom"), "Custom");
        assert_eq!(format_provider_name(""), "");
    }

    #[test]
    fn test_get_provider_description() {
        assert_eq!(
            get_provider_description("kokoro"),
            "Local ONNX-based TTS model with natural voices"
        );
        assert_eq!(
            get_provider_description("openai"),
            "OpenAI's cloud-based text-to-speech API"
        );
        assert_eq!(
            get_provider_description("avspeech"),
            "macOS built-in speech synthesis"
        );
        assert_eq!(get_provider_description("unknown"), "TTS provider");
    }

    #[test]
    fn test_providers_response_serialization() {
        let response = ProvidersResponse {
            providers: vec![
                ProviderInfo::Available(AvailableProvider {
                    id: "kokoro".to_string(),
                    name: "Kokoro 82M".to_string(),
                    description: "Local TTS".to_string(),
                    status: "available".to_string(),
                    voices: vec![VoiceInfo {
                        id: "af_bella".to_string(),
                        name: "Bella".to_string(),
                        language: "en-US".to_string(),
                        gender: Some(yappy_core::provider::VoiceGender::Female),
                        sample_url: None,
                    }],
                    supported_formats: vec![SupportedFormat {
                        codec: "pcm".to_string(),
                        sample_rates: vec![24000, 48000],
                    }],
                    options_schema: None,
                }),
                ProviderInfo::Unavailable(UnavailableProvider {
                    id: "openai".to_string(),
                    name: "OpenAI TTS".to_string(),
                    description: "Cloud TTS".to_string(),
                    status: "not_configured".to_string(),
                    reason: "API key not set".to_string(),
                }),
            ],
            default_provider: Some("kokoro".to_string()),
        };

        let json = serde_json::to_string(&response).expect("serialization should succeed");

        // Verify available provider has full fields
        assert!(json.contains("\"voices\""));
        assert!(json.contains("\"supported_formats\""));

        // Verify unavailable provider has reason
        assert!(json.contains("\"reason\":\"API key not set\""));

        // Verify default_provider is present
        assert!(json.contains("\"default_provider\":\"kokoro\""));
    }
}
