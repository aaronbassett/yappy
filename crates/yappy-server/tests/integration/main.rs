//! Integration tests for yappy-server
//!
//! This module serves as the entry point for integration tests.
//! Tests are organized into submodules by feature area.

mod common;
mod health_check;
mod shutdown;
mod ws_backpressure;
mod ws_basic;
mod ws_buffer;
mod ws_concurrent;
mod ws_errors;
mod ws_formats;
mod ws_providers;
mod ws_voices;
