//! Error types for Yappy

use serde::Serialize;

/// Provider-specific errors (non-fatal by default)
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// Synthesis failed
    #[error("Synthesis failed: {message}")]
    SynthesisFailed {
        /// Error message
        message: String,
    },

    /// Rate limited by provider
    #[error("Rate limited, retry after {retry_after_secs}s")]
    RateLimited {
        /// Seconds to wait before retry
        retry_after_secs: u32,
    },

    /// Provider timeout
    #[error("Provider timeout after {timeout_secs}s")]
    Timeout {
        /// Timeout duration
        timeout_secs: u32,
    },

    /// Invalid voice requested
    #[error("Invalid voice: {voice_id}")]
    InvalidVoice {
        /// Requested voice ID
        voice_id: String,
        /// Available voices
        available: Vec<String>,
    },

    /// Unsupported audio format
    #[error("Unsupported format: {format}")]
    UnsupportedFormat {
        /// Requested format
        format: String,
    },

    /// Provider not configured
    #[error("Provider not configured: {reason}")]
    NotConfigured {
        /// Reason not configured
        reason: String,
    },

    /// Cancelled by client disconnect
    #[error("Synthesis cancelled")]
    Cancelled,

    /// Internal provider error
    #[error("Internal provider error: {0}")]
    Internal(String),
}

impl ProviderError {
    /// Get error code for protocol messages
    pub const fn code(&self) -> &'static str {
        match self {
            Self::SynthesisFailed { .. } => "synthesis_failed",
            Self::RateLimited { .. } => "rate_limited",
            Self::Timeout { .. } => "provider_timeout",
            Self::InvalidVoice { .. } => "invalid_voice",
            Self::UnsupportedFormat { .. } => "unsupported_format",
            Self::NotConfigured { .. } => "not_configured",
            Self::Cancelled => "cancelled",
            Self::Internal(_) => "internal_error",
        }
    }

    /// Check if this is a fatal error (should close connection)
    pub const fn is_fatal(&self) -> bool {
        matches!(self, Self::Internal(_))
    }
}

/// Session-level errors (fatal - connection will be closed)
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// Protocol violation (malformed message, wrong sequence)
    #[error("Protocol violation: {message}")]
    ProtocolViolation {
        /// Description of violation
        message: String,
    },

    /// Provider unavailable
    #[error("Provider unavailable: {provider_id}")]
    ProviderUnavailable {
        /// Provider that was requested
        provider_id: String,
        /// Alternative providers available
        alternatives: Vec<String>,
    },

    /// Configuration error
    #[error("Configuration error: {message}")]
    ConfigError {
        /// Description of config issue
        message: String,
    },

    /// Internal server error
    #[error("Internal server error")]
    Internal,
}

impl SessionError {
    /// Get error code for protocol messages
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ProtocolViolation { .. } => "protocol_violation",
            Self::ProviderUnavailable { .. } => "provider_unavailable",
            Self::ConfigError { .. } => "config_error",
            Self::Internal => "internal_error",
        }
    }
}

/// Wire format for error messages sent to clients
#[derive(Debug, Clone, Serialize)]
pub struct ErrorResponse {
    /// Error code
    pub code: String,
    /// Human-readable message
    pub message: String,
    /// Whether this is a fatal error
    pub fatal: bool,
    /// Sentence index if applicable
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sentence_index: Option<u32>,
    /// Alternative options (e.g., available providers)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alternatives: Option<Vec<String>>,
}

impl From<&ProviderError> for ErrorResponse {
    fn from(err: &ProviderError) -> Self {
        Self {
            code: err.code().to_string(),
            message: err.to_string(),
            fatal: err.is_fatal(),
            sentence_index: None,
            alternatives: match err {
                ProviderError::InvalidVoice { available, .. } => Some(available.clone()),
                _ => None,
            },
        }
    }
}

impl From<&SessionError> for ErrorResponse {
    fn from(err: &SessionError) -> Self {
        Self {
            code: err.code().to_string(),
            message: err.to_string(),
            fatal: true,
            sentence_index: None,
            alternatives: match err {
                SessionError::ProviderUnavailable { alternatives, .. } => {
                    Some(alternatives.clone())
                }
                _ => None,
            },
        }
    }
}
