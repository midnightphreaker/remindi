use std::sync::Arc;

use anyhow::Context;
use remindi::{
    app::{AppState, run, shutdown_signal},
    clock::{SystemClock, UuidV7Generator},
    config::BootstrapConfig,
    db::DatabaseManager,
    http::{middleware::init_json_tracing, router::build_router},
    mcp::server::McpWorkload,
    remindi::RemindiService,
    scheduler::{AdapterProvider, Scheduler, SchedulerConfig},
    triggers::adapters::AdapterRegistry,
};
use secrecy::ExposeSecret;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

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
    let scheduler_service = Arc::new(RemindiService::new(
        Arc::clone(&database),
        state.bootstrap().owner_id(),
        state.bootstrap().mcp_token().expose_secret().as_bytes(),
        state.clock_shared(),
        state.ids_shared(),
    ));
    let adapters: Arc<dyn AdapterProvider> =
        Arc::new(AdapterRegistry::disabled(state.clock_shared()));
    let scheduler = Arc::new(
        Scheduler::new(
            Arc::clone(&database),
            scheduler_service,
            adapters,
            state.clock_shared(),
            format!("process-{}", state.ids().next_id().simple()),
            SchedulerConfig {
                poll_interval: std::time::Duration::from_secs(30),
                lease_duration: std::time::Duration::from_secs(90),
                adapter_timeout: std::time::Duration::from_secs(5),
                adapter_concurrency: 8,
                candidate_batch_size: 200,
            },
        )
        .context("scheduler workload startup failed")?,
    );
    let scheduler_cancel = CancellationToken::new();
    let scheduler_task = tokio::spawn({
        let scheduler = Arc::clone(&scheduler);
        let cancel = scheduler_cancel.clone();
        async move { scheduler.run(cancel).await }
    });
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
    scheduler_cancel.cancel();
    scheduler_task
        .await
        .context("scheduler workload task failed")?
        .context("scheduler workload shutdown failed")?;
    drop(state);
    drop(mcp);
    drop(scheduler);
    Arc::try_unwrap(database)
        .map_err(|_| anyhow::anyhow!("database still has active application references"))?
        .close()
        .await
        .context("database shutdown failed")?;
    result
}
