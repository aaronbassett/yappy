//! HTTP request handlers
//!
//! This module contains the Axum handlers for the Yappy TTS server's HTTP endpoints.

pub mod providers;

pub use providers::list_providers;
