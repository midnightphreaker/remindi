use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use axum::Router;
use tokio::net::TcpListener;

use crate::{
    clock::{Clock, IdGenerator},
    config::BootstrapConfig,
};

/// Shared state for the always-on control plane.
#[derive(Clone)]
pub struct AppState {
    bootstrap: Arc<BootstrapConfig>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    ready: Arc<AtomicBool>,
}

impl AppState {
    /// Assembles the small process-level seams used by the foundation.
    #[must_use]
    pub fn new(
        bootstrap: Arc<BootstrapConfig>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self {
            bootstrap,
            clock,
            ids,
            ready: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Returns immutable bootstrap configuration.
    #[must_use]
    pub fn bootstrap(&self) -> &BootstrapConfig {
        &self.bootstrap
    }

    /// Returns the configured clock seam.
    #[must_use]
    pub fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }

    /// Returns the configured identifier seam.
    #[must_use]
    pub fn ids(&self) -> &dyn IdGenerator {
        self.ids.as_ref()
    }

    /// Reports whether all currently implemented readiness checks pass.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    /// Changes readiness after startup validation or before maintenance.
    pub fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::Release);
    }
}

/// Serves one Axum router and drains accepted requests after shutdown begins.
///
/// # Errors
///
/// Returns an I/O error if the listener fails while serving.
pub async fn run(
    listener: TcpListener,
    router: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await
}

/// Waits for SIGINT or SIGTERM.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(event = "signal_registration_failed", %error);
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(event = "signal_registration_failed", %error);
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }

    tracing::info!(event = "shutdown_started");
}
