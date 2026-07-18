use std::sync::Arc;

use anyhow::Context;
use remindi::{
    app::{AppState, run, shutdown_signal},
    clock::{SystemClock, UuidV7Generator},
    config::BootstrapConfig,
    db::DatabaseManager,
    http::{middleware::init_json_tracing, router::build_router},
    mcp::server::McpWorkload,
};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let bootstrap = BootstrapConfig::from_env().context("bootstrap configuration is invalid")?;
    init_json_tracing(&bootstrap).context("structured logging initialization failed")?;
    let address = bootstrap.listener_address();
    let database = Arc::new(
        DatabaseManager::open(bootstrap.database_path())
            .await
            .context("database startup failed")?,
    );
    let state = AppState::new(
        Arc::new(bootstrap),
        Arc::new(SystemClock),
        Arc::new(UuidV7Generator),
    )
    .with_database(Arc::clone(&database));
    let mcp = Arc::new(McpWorkload::new(&state).context("MCP workload startup failed")?);
    let state = state.with_mcp(Arc::clone(&mcp));
    let listener = TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind the fixed listener at {address}"))?;

    state.set_ready(true);
    tracing::info!(event = "control_plane_started", %address);
    let result = run(listener, build_router(state.clone()), shutdown_signal())
        .await
        .context("control plane failed");
    state.set_ready(false);
    mcp.stop().context("MCP workload shutdown failed")?;
    drop(state);
    drop(mcp);
    Arc::try_unwrap(database)
        .map_err(|_| anyhow::anyhow!("database still has active application references"))?
        .close()
        .await
        .context("database shutdown failed")?;
    result
}
