use std::sync::Arc;

use anyhow::Context;
use remindi::{
    app::{AppState, run, shutdown_signal},
    clock::{SystemClock, UuidV7Generator},
    config::BootstrapConfig,
    http::{middleware::init_json_tracing, router::build_router},
};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let bootstrap = BootstrapConfig::from_env().context("bootstrap configuration is invalid")?;
    init_json_tracing(&bootstrap).context("structured logging initialization failed")?;
    let address = bootstrap.listener_address();
    let state = AppState::new(
        Arc::new(bootstrap),
        Arc::new(SystemClock),
        Arc::new(UuidV7Generator),
    );
    let listener = TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind the fixed listener at {address}"))?;

    tracing::info!(event = "control_plane_started", %address);
    run(listener, build_router(state), shutdown_signal())
        .await
        .context("control plane failed")
}
