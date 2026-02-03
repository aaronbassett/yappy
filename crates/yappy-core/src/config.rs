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

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            log_level: default_log_level(),
            idle_timeout_secs: default_idle_timeout(),
            synthesis_timeout_secs: default_synthesis_timeout(),
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
        }
    }
}

impl Config {
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
