//! Configuration types

use serde::Deserialize;
use std::time::Duration;

/// Root configuration structure
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Server configuration
    #[serde(default)]
    pub server: ServerConfig,

    /// Provider configuration
    pub providers: ProvidersConfig,

    /// Buffer configuration
    #[serde(default)]
    pub buffer: BufferConfigToml,
}

/// Server configuration
#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    /// Bind address (default: "127.0.0.1")
    #[serde(default = "default_host")]
    pub host: String,

    /// Port (default: 3000)
    #[serde(default = "default_port")]
    pub port: u16,

    /// Log level (default: "info")
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// Idle connection timeout in seconds (default: 300 = 5 minutes)
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout_secs: u64,

    /// Synthesis request timeout in seconds (default: 30)
    #[serde(default = "default_synthesis_timeout")]
    pub synthesis_timeout_secs: u64,

    /// Audio channel capacity for backpressure (default: 32)
    ///
    /// This controls the bounded channel capacity between the TTS synthesis task
    /// and the WebSocket sender. When the channel is full, synthesis pauses until
    /// the client consumes audio frames. Valid range: 1-256.
    #[serde(default = "default_audio_channel_capacity")]
    pub audio_channel_capacity: usize,
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

const fn default_port() -> u16 {
    3000
}

fn default_log_level() -> String {
    "info".to_string()
}

const fn default_idle_timeout() -> u64 {
    300
}

const fn default_synthesis_timeout() -> u64 {
    30
}

const fn default_audio_channel_capacity() -> usize {
    32
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            log_level: default_log_level(),
            idle_timeout_secs: default_idle_timeout(),
            synthesis_timeout_secs: default_synthesis_timeout(),
            audio_channel_capacity: default_audio_channel_capacity(),
        }
    }
}

impl ServerConfig {
    /// Get bind address as "host:port"
    pub fn bind_address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Get idle timeout as Duration
    pub const fn idle_timeout(&self) -> Duration {
        Duration::from_secs(self.idle_timeout_secs)
    }

    /// Get synthesis timeout as Duration
    pub const fn synthesis_timeout(&self) -> Duration {
        Duration::from_secs(self.synthesis_timeout_secs)
    }
}

/// Provider configuration section
#[derive(Debug, Clone, Deserialize)]
pub struct ProvidersConfig {
    /// Default provider ID
    pub default: String,

    /// `OpenAI` provider settings
    pub openai: Option<OpenAiConfig>,

    /// Kokoro provider settings
    pub kokoro: Option<KokoroConfig>,

    /// `AVSpeech` provider settings (macOS only)
    pub avspeech: Option<AvSpeechConfig>,
}

/// `OpenAI` TTS provider settings
#[derive(Debug, Clone, Deserialize)]
pub struct OpenAiConfig {
    /// API key (supports `$ENV_VAR` syntax)
    pub api_key: String,

    /// Model to use (default: "tts-1")
    #[serde(default = "default_openai_model")]
    pub model: String,
}

fn default_openai_model() -> String {
    "tts-1".to_string()
}

impl OpenAiConfig {
    /// Resolve API key from environment variable if needed
    pub fn resolve_api_key(&self) -> Option<String> {
        if self.api_key.starts_with('$') {
            let var_name = &self.api_key[1..];
            std::env::var(var_name).ok()
        } else {
            Some(self.api_key.clone())
        }
    }
}

/// Kokoro ONNX provider settings
#[derive(Debug, Clone, Deserialize)]
pub struct KokoroConfig {
    /// Path to ONNX model file (or "auto" for HF download)
    #[serde(default = "default_auto")]
    pub model_path: String,

    /// Path to voices file (or "auto" for HF download)
    #[serde(default = "default_auto")]
    pub voices_path: String,
}

fn default_auto() -> String {
    "auto".to_string()
}

impl Default for KokoroConfig {
    fn default() -> Self {
        Self {
            model_path: default_auto(),
            voices_path: default_auto(),
        }
    }
}

/// macOS `AVSpeechSynthesizer` settings
#[derive(Debug, Clone, Deserialize)]
pub struct AvSpeechConfig {
    /// Whether to enable (default: true on macOS)
    #[serde(default = "default_true")]
    pub enabled: bool,
}

const fn default_true() -> bool {
    true
}

impl Default for AvSpeechConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Buffer configuration (TOML format)
#[derive(Debug, Clone, Deserialize)]
pub struct BufferConfigToml {
    /// Flush timeout in milliseconds (default: 500)
    #[serde(default = "default_flush_timeout")]
    pub flush_timeout_ms: u64,

    /// Maximum buffer size in bytes (default: 4096)
    #[serde(default = "default_max_size")]
    pub max_size_bytes: usize,
}

const fn default_flush_timeout() -> u64 {
    500
}

const fn default_max_size() -> usize {
    4096
}

impl Default for BufferConfigToml {
    fn default() -> Self {
        Self {
            flush_timeout_ms: default_flush_timeout(),
            max_size_bytes: default_max_size(),
        }
    }
}

impl From<BufferConfigToml> for crate::buffer::BufferConfig {
    fn from(toml: BufferConfigToml) -> Self {
        Self {
            flush_timeout: Duration::from_millis(toml.flush_timeout_ms),
            max_size: toml.max_size_bytes,
            code_block_mode: crate::session::CodeBlockMode::default(),
        }
    }
}

impl Config {
    /// Validate the configuration
    ///
    /// This method checks that all configuration values are valid before
    /// the server starts. It does NOT check if files exist (that's runtime validation).
    ///
    /// # Errors
    ///
    /// Returns `ConfigValidationError` if any configuration value is invalid.
    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        self.validate_server()?;
        self.validate_providers()?;
        self.validate_buffer()?;
        Ok(())
    }

    /// Validate server configuration
    fn validate_server(&self) -> Result<(), ConfigValidationError> {
        // Port validation: port 0 is invalid (it means "any available port" which
        // is not appropriate for a configured server)
        if self.server.port == 0 {
            return Err(ConfigValidationError::InvalidPort(self.server.port));
        }

        // Log level validation
        let log_level_lower = self.server.log_level.to_lowercase();
        if !VALID_LOG_LEVELS.contains(&log_level_lower.as_str()) {
            return Err(ConfigValidationError::InvalidLogLevel(
                self.server.log_level.clone(),
            ));
        }

        // Idle timeout validation
        if self.server.idle_timeout_secs == 0 {
            return Err(ConfigValidationError::InvalidTimeout {
                field: "idle_timeout_secs".to_string(),
            });
        }

        // Synthesis timeout validation
        if self.server.synthesis_timeout_secs == 0 {
            return Err(ConfigValidationError::InvalidTimeout {
                field: "synthesis_timeout_secs".to_string(),
            });
        }

        // Audio channel capacity validation (1-256)
        if self.server.audio_channel_capacity == 0 {
            return Err(ConfigValidationError::InvalidAudioChannelCapacity {
                value: 0,
                reason: "must be > 0".to_string(),
            });
        }
        if self.server.audio_channel_capacity > MAX_AUDIO_CHANNEL_CAPACITY {
            return Err(ConfigValidationError::InvalidAudioChannelCapacity {
                value: self.server.audio_channel_capacity,
                reason: format!("must be <= {MAX_AUDIO_CHANNEL_CAPACITY}"),
            });
        }

        Ok(())
    }

    /// Validate provider configuration
    fn validate_providers(&self) -> Result<(), ConfigValidationError> {
        // Default provider validation
        let default_lower = self.providers.default.to_lowercase();
        if !VALID_PROVIDERS.contains(&default_lower.as_str()) {
            return Err(ConfigValidationError::InvalidDefaultProvider(
                self.providers.default.clone(),
            ));
        }

        // OpenAI API key validation
        if let Some(ref openai) = self.providers.openai {
            // Check if api_key is empty (after trimming whitespace)
            // Also check for empty env var reference like "$" without a var name
            let key = openai.api_key.trim();
            if key.is_empty() || key == "$" {
                return Err(ConfigValidationError::EmptyApiKey);
            }
        }

        // Note: Kokoro path validation is intentionally not done here.
        // If the path is not "auto", actual file existence is checked at runtime.

        Ok(())
    }

    /// Validate buffer configuration
    fn validate_buffer(&self) -> Result<(), ConfigValidationError> {
        // Flush timeout validation
        if self.buffer.flush_timeout_ms == 0 {
            return Err(ConfigValidationError::InvalidBufferConfig(
                "flush_timeout_ms must be > 0".to_string(),
            ));
        }

        // Max size validation
        if self.buffer.max_size_bytes == 0 {
            return Err(ConfigValidationError::InvalidBufferConfig(
                "max_size_bytes must be > 0".to_string(),
            ));
        }

        if self.buffer.max_size_bytes > MAX_BUFFER_SIZE {
            return Err(ConfigValidationError::InvalidBufferConfig(format!(
                "max_size_bytes must be <= {} (1MB), got {}",
                MAX_BUFFER_SIZE, self.buffer.max_size_bytes
            )));
        }

        Ok(())
    }

    /// Load configuration from a TOML file
    pub fn load(path: &std::path::Path) -> Result<Self, ConfigError> {
        let contents = std::fs::read_to_string(path).map_err(|e| ConfigError::Read {
            path: path.to_path_buf(),
            source: e,
        })?;

        toml::from_str(&contents).map_err(|e| ConfigError::Parse {
            path: path.to_path_buf(),
            source: e,
        })
    }

    /// Load configuration from default paths
    ///
    /// Checks in order:
    /// 1. ./yappy.toml
    /// 2. ~/.config/yappy/config.toml
    pub fn load_default() -> Result<Self, ConfigError> {
        let candidates = [
            std::path::PathBuf::from("yappy.toml"),
            dirs::config_dir()
                .map(|p| p.join("yappy/config.toml"))
                .unwrap_or_default(),
        ];

        for path in candidates {
            if path.exists() {
                return Self::load(&path);
            }
        }

        Err(ConfigError::NotFound)
    }
}

/// Maximum allowed buffer size (1MB)
const MAX_BUFFER_SIZE: usize = 1_048_576;

/// Maximum allowed audio channel capacity
const MAX_AUDIO_CHANNEL_CAPACITY: usize = 256;

/// Valid log levels
const VALID_LOG_LEVELS: &[&str] = &["error", "warn", "info", "debug", "trace"];

/// Valid provider names
const VALID_PROVIDERS: &[&str] = &["kokoro", "openai", "avspeech"];

/// Configuration validation errors
#[derive(Debug, thiserror::Error)]
pub enum ConfigValidationError {
    /// Invalid port number
    #[error("Invalid port: {0}")]
    InvalidPort(u16),

    /// Invalid log level
    #[error("Invalid log level: {0}. Must be one of: error, warn, info, debug, trace")]
    InvalidLogLevel(String),

    /// Invalid default provider
    #[error("Invalid default provider: {0}. Must be one of: kokoro, openai, avspeech")]
    InvalidDefaultProvider(String),

    /// Invalid timeout value
    #[error("Invalid timeout: {field} must be > 0")]
    InvalidTimeout {
        /// The field name with the invalid timeout
        field: String,
    },

    /// Invalid buffer configuration
    #[error("Invalid buffer config: {0}")]
    InvalidBufferConfig(String),

    /// Invalid audio channel capacity
    #[error("Invalid audio_channel_capacity: {value} ({reason})")]
    InvalidAudioChannelCapacity {
        /// The invalid value
        value: usize,
        /// Why it's invalid
        reason: String,
    },

    /// Empty API key for `OpenAI`
    #[error("OpenAI API key is empty or not set")]
    EmptyApiKey,
}

/// Configuration loading errors
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Configuration file not found
    #[error("Configuration file not found. Create yappy.toml or ~/.config/yappy/config.toml")]
    NotFound,

    /// Failed to read configuration file
    #[error("Failed to read configuration file {path}: {source}")]
    Read {
        /// Path to the file
        path: std::path::PathBuf,
        /// IO error
        source: std::io::Error,
    },

    /// Failed to parse configuration file
    #[error("Failed to parse configuration file {path}: {source}")]
    Parse {
        /// Path to the file
        path: std::path::PathBuf,
        /// Parse error
        source: toml::de::Error,
    },
}

// External dependency for config directory
mod dirs {
    pub fn config_dir() -> Option<std::path::PathBuf> {
        std::env::var("HOME")
            .ok()
            .map(|h| std::path::PathBuf::from(h).join(".config"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a valid base config for testing
    fn valid_config() -> Config {
        Config {
            server: ServerConfig::default(),
            providers: ProvidersConfig {
                default: "kokoro".to_string(),
                openai: None,
                kokoro: Some(KokoroConfig::default()),
                avspeech: None,
            },
            buffer: BufferConfigToml::default(),
        }
    }

    // ========== Server Config Validation Tests ==========

    #[test]
    fn test_valid_config_passes_validation() {
        let config = valid_config();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_invalid_port_zero() {
        let mut config = valid_config();
        config.server.port = 0;

        let result = config.validate();
        assert!(matches!(result, Err(ConfigValidationError::InvalidPort(0))));
    }

    #[test]
    fn test_valid_port_one() {
        let mut config = valid_config();
        config.server.port = 1;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_valid_port_max() {
        let mut config = valid_config();
        config.server.port = 65535;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_invalid_log_level() {
        let mut config = valid_config();
        config.server.log_level = "invalid".to_string();

        let result = config.validate();
        assert!(matches!(
            result,
            Err(ConfigValidationError::InvalidLogLevel(ref level)) if level == "invalid"
        ));
    }

    #[test]
    fn test_valid_log_levels() {
        for level in &[
            "error", "warn", "info", "debug", "trace", "ERROR", "WARN", "Info",
        ] {
            let mut config = valid_config();
            config.server.log_level = (*level).to_string();
            assert!(
                config.validate().is_ok(),
                "Log level '{level}' should be valid"
            );
        }
    }

    #[test]
    fn test_invalid_idle_timeout_zero() {
        let mut config = valid_config();
        config.server.idle_timeout_secs = 0;

        let result = config.validate();
        assert!(matches!(
            result,
            Err(ConfigValidationError::InvalidTimeout { ref field }) if field == "idle_timeout_secs"
        ));
    }

    #[test]
    fn test_invalid_synthesis_timeout_zero() {
        let mut config = valid_config();
        config.server.synthesis_timeout_secs = 0;

        let result = config.validate();
        assert!(matches!(
            result,
            Err(ConfigValidationError::InvalidTimeout { ref field }) if field == "synthesis_timeout_secs"
        ));
    }

    // ========== Provider Config Validation Tests ==========

    #[test]
    fn test_invalid_default_provider() {
        let mut config = valid_config();
        config.providers.default = "unknown".to_string();

        let result = config.validate();
        assert!(matches!(
            result,
            Err(ConfigValidationError::InvalidDefaultProvider(ref provider)) if provider == "unknown"
        ));
    }

    #[test]
    fn test_valid_default_providers() {
        for provider in &[
            "kokoro", "openai", "avspeech", "KOKORO", "OpenAI", "AVSpeech",
        ] {
            let mut config = valid_config();
            config.providers.default = (*provider).to_string();
            assert!(
                config.validate().is_ok(),
                "Provider '{provider}' should be valid"
            );
        }
    }

    #[test]
    fn test_openai_empty_api_key() {
        let mut config = valid_config();
        config.providers.openai = Some(OpenAiConfig {
            api_key: String::new(),
            model: "tts-1".to_string(),
        });

        let result = config.validate();
        assert!(matches!(result, Err(ConfigValidationError::EmptyApiKey)));
    }

    #[test]
    fn test_openai_whitespace_only_api_key() {
        let mut config = valid_config();
        config.providers.openai = Some(OpenAiConfig {
            api_key: "   ".to_string(),
            model: "tts-1".to_string(),
        });

        let result = config.validate();
        assert!(matches!(result, Err(ConfigValidationError::EmptyApiKey)));
    }

    #[test]
    fn test_openai_empty_env_var_reference() {
        let mut config = valid_config();
        config.providers.openai = Some(OpenAiConfig {
            api_key: "$".to_string(),
            model: "tts-1".to_string(),
        });

        let result = config.validate();
        assert!(matches!(result, Err(ConfigValidationError::EmptyApiKey)));
    }

    #[test]
    fn test_openai_valid_api_key() {
        let mut config = valid_config();
        config.providers.openai = Some(OpenAiConfig {
            api_key: "sk-test-key".to_string(),
            model: "tts-1".to_string(),
        });

        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_openai_valid_env_var_reference() {
        let mut config = valid_config();
        config.providers.openai = Some(OpenAiConfig {
            api_key: "$OPENAI_API_KEY".to_string(),
            model: "tts-1".to_string(),
        });

        // This should pass validation - the env var resolution happens at runtime
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_kokoro_custom_path_passes_validation() {
        let mut config = valid_config();
        config.providers.kokoro = Some(KokoroConfig {
            model_path: "/path/to/model.onnx".to_string(),
            voices_path: "/path/to/voices.bin".to_string(),
        });

        // Path validation happens at runtime, not during config validation
        assert!(config.validate().is_ok());
    }

    // ========== Buffer Config Validation Tests ==========

    #[test]
    fn test_invalid_flush_timeout_zero() {
        let mut config = valid_config();
        config.buffer.flush_timeout_ms = 0;

        let result = config.validate();
        assert!(matches!(
            result,
            Err(ConfigValidationError::InvalidBufferConfig(ref msg)) if msg.contains("flush_timeout_ms")
        ));
    }

    #[test]
    fn test_invalid_max_size_zero() {
        let mut config = valid_config();
        config.buffer.max_size_bytes = 0;

        let result = config.validate();
        assert!(matches!(
            result,
            Err(ConfigValidationError::InvalidBufferConfig(ref msg)) if msg.contains("max_size_bytes must be > 0")
        ));
    }

    #[test]
    fn test_invalid_max_size_exceeds_limit() {
        let mut config = valid_config();
        config.buffer.max_size_bytes = MAX_BUFFER_SIZE + 1;

        let result = config.validate();
        assert!(matches!(
            result,
            Err(ConfigValidationError::InvalidBufferConfig(ref msg)) if msg.contains("1MB")
        ));
    }

    #[test]
    fn test_valid_max_size_at_limit() {
        let mut config = valid_config();
        config.buffer.max_size_bytes = MAX_BUFFER_SIZE;

        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_valid_buffer_config() {
        let mut config = valid_config();
        config.buffer.flush_timeout_ms = 1000;
        config.buffer.max_size_bytes = 8192;

        assert!(config.validate().is_ok());
    }

    // ========== Error Display Tests ==========

    #[test]
    fn test_error_display_invalid_port() {
        let err = ConfigValidationError::InvalidPort(0);
        assert_eq!(err.to_string(), "Invalid port: 0");
    }

    #[test]
    fn test_error_display_invalid_log_level() {
        let err = ConfigValidationError::InvalidLogLevel("foo".to_string());
        assert_eq!(
            err.to_string(),
            "Invalid log level: foo. Must be one of: error, warn, info, debug, trace"
        );
    }

    #[test]
    fn test_error_display_invalid_provider() {
        let err = ConfigValidationError::InvalidDefaultProvider("bar".to_string());
        assert_eq!(
            err.to_string(),
            "Invalid default provider: bar. Must be one of: kokoro, openai, avspeech"
        );
    }

    #[test]
    fn test_error_display_invalid_timeout() {
        let err = ConfigValidationError::InvalidTimeout {
            field: "synthesis_timeout_secs".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "Invalid timeout: synthesis_timeout_secs must be > 0"
        );
    }

    #[test]
    fn test_error_display_invalid_buffer() {
        let err =
            ConfigValidationError::InvalidBufferConfig("max_size_bytes must be > 0".to_string());
        assert_eq!(
            err.to_string(),
            "Invalid buffer config: max_size_bytes must be > 0"
        );
    }

    #[test]
    fn test_error_display_empty_api_key() {
        let err = ConfigValidationError::EmptyApiKey;
        assert_eq!(err.to_string(), "OpenAI API key is empty or not set");
    }
}
