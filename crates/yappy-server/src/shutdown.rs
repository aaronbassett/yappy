//! Graceful shutdown coordination for the Yappy TTS server
//!
//! This module provides graceful shutdown handling per FR-027 and SC-007:
//! - SIGTERM/SIGINT signal handling
//! - Tracking of active WebSocket sessions
//! - Coordinated drain period with timeout
//! - Cancellation token propagation to providers
//!
//! # Shutdown Flow
//!
//! 1. Server receives SIGTERM or SIGINT signal
//! 2. `ShutdownCoordinator` is notified, triggering shutdown
//! 3. Server stops accepting new connections (via `axum::serve::with_graceful_shutdown`)
//! 4. Active sessions are notified via cancellation token
//! 5. Sessions complete current synthesis, send `audio.done`, then close
//! 6. After all sessions close OR timeout (5 seconds), server exits
//!
//! # Session Tracking
//!
//! Each WebSocket session registers with the coordinator on connect and
//! unregisters on disconnect. This allows the coordinator to:
//! - Track how many sessions are active
//! - Wait for all sessions to complete gracefully
//! - Force-terminate after the drain timeout

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Default drain timeout for graceful shutdown (SC-007)
pub const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Coordinator for graceful server shutdown
///
/// This struct manages the shutdown lifecycle, including:
/// - A cancellation token that signals all sessions to stop
/// - Tracking of active session count
/// - Notification when all sessions have completed
///
/// # Thread Safety
///
/// `ShutdownCoordinator` is designed to be shared across handlers via `Arc`.
/// All operations are thread-safe using atomic operations and lock-free primitives.
///
/// # Example
///
/// ```ignore
/// let coordinator = ShutdownCoordinator::new();
///
/// // In WebSocket handler:
/// let guard = coordinator.register_session();
/// // ... handle session ...
/// // guard dropped on disconnect, unregistering session
///
/// // In main:
/// coordinator.initiate_shutdown();
/// coordinator.wait_for_drain(Duration::from_secs(5)).await;
/// ```
#[derive(Debug)]
pub struct ShutdownCoordinator {
    /// Cancellation token for signaling shutdown to all sessions
    shutdown_token: CancellationToken,

    /// Number of currently active sessions
    active_sessions: AtomicUsize,

    /// Notifier for when all sessions have completed
    all_sessions_done: Notify,
}

impl Default for ShutdownCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl ShutdownCoordinator {
    /// Create a new shutdown coordinator
    pub fn new() -> Self {
        Self {
            shutdown_token: CancellationToken::new(),
            active_sessions: AtomicUsize::new(0),
            all_sessions_done: Notify::new(),
        }
    }

    /// Get the cancellation token for this coordinator
    ///
    /// Sessions should use this token (or a child of it) to check for
    /// shutdown signals and cancel ongoing work.
    pub fn token(&self) -> CancellationToken {
        self.shutdown_token.clone()
    }

    /// Check if shutdown has been initiated
    pub fn is_shutting_down(&self) -> bool {
        self.shutdown_token.is_cancelled()
    }

    /// Get the current number of active sessions
    pub fn active_session_count(&self) -> usize {
        self.active_sessions.load(Ordering::Relaxed)
    }

    /// Register a new session and return a guard that will unregister on drop
    ///
    /// This should be called when a WebSocket connection is established.
    /// The returned `SessionGuard` must be held for the lifetime of the session.
    ///
    /// # Returns
    ///
    /// A `SessionGuard` that automatically unregisters the session when dropped,
    /// along with a child cancellation token for this specific session.
    pub fn register_session(self: &Arc<Self>) -> SessionGuard {
        let previous = self.active_sessions.fetch_add(1, Ordering::SeqCst);
        debug!(
            active_sessions = previous + 1,
            "Session registered with shutdown coordinator"
        );

        // Create a child token for this session
        let session_token = self.shutdown_token.child_token();

        SessionGuard {
            coordinator: Arc::clone(self),
            token: session_token,
        }
    }

    /// Initiate graceful shutdown
    ///
    /// This cancels the shutdown token, signaling all sessions to begin
    /// their graceful shutdown process.
    pub fn initiate_shutdown(&self) {
        if self.shutdown_token.is_cancelled() {
            debug!("Shutdown already initiated");
            return;
        }

        let active = self.active_sessions.load(Ordering::Relaxed);
        info!(
            active_sessions = active,
            "Initiating graceful shutdown"
        );

        self.shutdown_token.cancel();
    }

    /// Wait for all sessions to drain with a timeout
    ///
    /// This method blocks until either:
    /// - All active sessions have completed and unregistered
    /// - The timeout expires
    ///
    /// # Arguments
    ///
    /// * `timeout` - Maximum time to wait for sessions to drain
    ///
    /// # Returns
    ///
    /// `true` if all sessions completed gracefully, `false` if timeout expired
    /// with sessions still active.
    pub async fn wait_for_drain(&self, timeout: Duration) -> bool {
        let start = std::time::Instant::now();

        // Fast path: no active sessions
        if self.active_sessions.load(Ordering::Relaxed) == 0 {
            info!("No active sessions, shutdown complete");
            return true;
        }

        info!(
            active_sessions = self.active_sessions.load(Ordering::Relaxed),
            timeout_secs = timeout.as_secs(),
            "Waiting for active sessions to drain"
        );

        // Wait for notification with timeout
        let result = tokio::time::timeout(timeout, async {
            loop {
                // Check if all sessions are done
                if self.active_sessions.load(Ordering::Relaxed) == 0 {
                    return true;
                }

                // Wait for notification or check periodically
                tokio::select! {
                    () = self.all_sessions_done.notified() => {
                        // Re-check session count after notification
                        if self.active_sessions.load(Ordering::Relaxed) == 0 {
                            return true;
                        }
                    }
                    () = tokio::time::sleep(Duration::from_millis(100)) => {
                        // Periodic check in case we missed a notification
                    }
                }
            }
        })
        .await;

        match result {
            Ok(true) => {
                let elapsed = start.elapsed();
                #[allow(clippy::cast_possible_truncation)]
                let elapsed_ms = elapsed.as_millis() as u64;
                info!(
                    elapsed_ms,
                    "All sessions drained gracefully"
                );
                true
            }
            Ok(false) => {
                // This shouldn't happen given our loop logic
                warn!("Unexpected drain result");
                false
            }
            Err(_timeout) => {
                let remaining = self.active_sessions.load(Ordering::Relaxed);
                warn!(
                    remaining_sessions = remaining,
                    timeout_secs = timeout.as_secs(),
                    "Drain timeout expired with sessions still active"
                );
                false
            }
        }
    }

    /// Internal: unregister a session (called by `SessionGuard` on drop)
    fn unregister_session(&self) {
        let previous = self.active_sessions.fetch_sub(1, Ordering::SeqCst);
        debug!(
            active_sessions = previous - 1,
            "Session unregistered from shutdown coordinator"
        );

        // If this was the last session and we're shutting down, notify waiters
        if previous == 1 {
            self.all_sessions_done.notify_waiters();
        }
    }
}

/// RAII guard for session registration
///
/// This guard is returned by `ShutdownCoordinator::register_session` and
/// automatically unregisters the session when dropped. It also provides
/// access to a cancellation token for this specific session.
///
/// # Usage
///
/// Hold this guard for the lifetime of the WebSocket connection. The session
/// will be unregistered when the guard is dropped (either normally or due to
/// panic/cancellation).
#[derive(Debug)]
pub struct SessionGuard {
    /// Reference to the coordinator for unregistration
    coordinator: Arc<ShutdownCoordinator>,

    /// Cancellation token for this session (child of the coordinator's token)
    token: CancellationToken,
}

impl SessionGuard {
    /// Get the cancellation token for this session
    ///
    /// This token is cancelled when:
    /// - The server initiates shutdown (via coordinator)
    /// - The session itself is explicitly cancelled
    ///
    /// Sessions should pass this token (or a child of it) to provider
    /// `synthesize()` calls to support cooperative cancellation.
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    /// Check if this session should shut down
    ///
    /// Returns `true` if the coordinator has initiated shutdown.
    pub fn is_shutting_down(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Create a child token for a specific synthesis operation
    ///
    /// This allows cancelling individual synthesis operations without
    /// affecting the entire session (e.g., when moving to next sentence).
    pub fn child_token(&self) -> CancellationToken {
        self.token.child_token()
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.coordinator.unregister_session();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coordinator_new() {
        let coordinator = ShutdownCoordinator::new();
        assert!(!coordinator.is_shutting_down());
        assert_eq!(coordinator.active_session_count(), 0);
    }

    #[test]
    fn test_register_session_increments_count() {
        let coordinator = Arc::new(ShutdownCoordinator::new());

        assert_eq!(coordinator.active_session_count(), 0);

        let guard1 = coordinator.register_session();
        assert_eq!(coordinator.active_session_count(), 1);

        let guard2 = coordinator.register_session();
        assert_eq!(coordinator.active_session_count(), 2);

        drop(guard1);
        assert_eq!(coordinator.active_session_count(), 1);

        drop(guard2);
        assert_eq!(coordinator.active_session_count(), 0);
    }

    #[test]
    fn test_initiate_shutdown_cancels_token() {
        let coordinator = ShutdownCoordinator::new();

        assert!(!coordinator.is_shutting_down());
        assert!(!coordinator.token().is_cancelled());

        coordinator.initiate_shutdown();

        assert!(coordinator.is_shutting_down());
        assert!(coordinator.token().is_cancelled());
    }

    #[test]
    fn test_session_token_cancelled_on_shutdown() {
        let coordinator = Arc::new(ShutdownCoordinator::new());
        let guard = coordinator.register_session();

        assert!(!guard.is_shutting_down());
        assert!(!guard.token().is_cancelled());

        coordinator.initiate_shutdown();

        assert!(guard.is_shutting_down());
        assert!(guard.token().is_cancelled());
    }

    #[test]
    fn test_child_token_cancelled_on_shutdown() {
        let coordinator = Arc::new(ShutdownCoordinator::new());
        let guard = coordinator.register_session();
        let child = guard.child_token();

        assert!(!child.is_cancelled());

        coordinator.initiate_shutdown();

        assert!(child.is_cancelled());
    }

    #[tokio::test]
    async fn test_wait_for_drain_no_sessions() {
        let coordinator = ShutdownCoordinator::new();
        coordinator.initiate_shutdown();

        let result = coordinator.wait_for_drain(Duration::from_millis(100)).await;
        assert!(result);
    }

    #[tokio::test]
    async fn test_wait_for_drain_sessions_complete() {
        let coordinator = Arc::new(ShutdownCoordinator::new());
        let guard = coordinator.register_session();

        coordinator.initiate_shutdown();

        // Spawn a task that drops the guard after a short delay
        let coord_clone = Arc::clone(&coordinator);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(guard);
        });

        let result = coord_clone.wait_for_drain(Duration::from_secs(1)).await;
        assert!(result);
        assert_eq!(coord_clone.active_session_count(), 0);
    }

    #[tokio::test]
    async fn test_wait_for_drain_timeout() {
        let coordinator = Arc::new(ShutdownCoordinator::new());
        let _guard = coordinator.register_session(); // Hold the guard to keep session active

        coordinator.initiate_shutdown();

        let result = coordinator.wait_for_drain(Duration::from_millis(50)).await;
        assert!(!result);
        assert_eq!(coordinator.active_session_count(), 1);
    }

    #[test]
    fn test_multiple_shutdown_calls_idempotent() {
        let coordinator = ShutdownCoordinator::new();

        coordinator.initiate_shutdown();
        assert!(coordinator.is_shutting_down());

        // Second call should not panic
        coordinator.initiate_shutdown();
        assert!(coordinator.is_shutting_down());
    }
}
