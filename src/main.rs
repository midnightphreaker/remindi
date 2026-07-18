use std::sync::Arc;

use anyhow::Context;
use remindi::{
    admin::{
        AdminService,
        backup::{BackupManager, RestoreManager},
        settings::RuntimeSetting,
        workloads::WorkloadController,
    },
    app::{AppState, run, shutdown_signal},
    auth::web_session::{WebMode, WebSessionManager},
    clock::{SystemClock, UuidV7Generator},
    config::BootstrapConfig,
    db::DatabaseManager,
    http::{api::WebApiState, middleware::init_json_tracing, router::build_router},
    mcp::server::McpWorkload,
    remindi::RemindiService,
    scheduler::{AdapterProvider, Scheduler, SchedulerConfig, SchedulerWorkload},
    webui::{AssetOverrides, WebUiAssets},
};
use secrecy::ExposeSecret;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let bootstrap = BootstrapConfig::from_env().context("bootstrap configuration is invalid")?;
    init_json_tracing(&bootstrap).context("structured logging initialization failed")?;
    let address = bootstrap.listener_address();
    RestoreManager::recover_interrupted(
        bootstrap.database_path(),
        bootstrap.backup_directory(),
        bootstrap.owner_id(),
        time::OffsetDateTime::now_utc(),
    )
    .await
    .context("interrupted restore recovery failed")?;
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
    let web_sessions = WebSessionManager::from_config(state.bootstrap())
        .map_err(|_| anyhow::anyhow!("WebUI session startup failed"))?;
    if web_sessions.mode() == WebMode::Unauthenticated {
        tracing::warn!(event = "webui_authentication_disabled");
    }
    let web_service = Arc::new(RemindiService::new(
        Arc::clone(&database),
        state.bootstrap().owner_id(),
        state.bootstrap().mcp_token().expose_secret().as_bytes(),
        state.clock_shared(),
        state.ids_shared(),
    ));
    let webui_assets = if state.bootstrap().webui_enabled() {
        Some(Arc::new(
            WebUiAssets::load(
                state.bootstrap().webui_title(),
                AssetOverrides {
                    custom_css: state
                        .bootstrap()
                        .webui_custom_css_file()
                        .map(ToOwned::to_owned),
                    logo: state.bootstrap().webui_logo_file().map(ToOwned::to_owned),
                    favicon: state
                        .bootstrap()
                        .webui_favicon_file()
                        .map(ToOwned::to_owned),
                },
            )
            .context("WebUI asset startup failed")?,
        ))
    } else {
        None
    };
    let scheduler_service = Arc::new(RemindiService::new(
        Arc::clone(&database),
        state.bootstrap().owner_id(),
        state.bootstrap().mcp_token().expose_secret().as_bytes(),
        state.clock_shared(),
        state.ids_shared(),
    ));
    let administration = Arc::new(
        AdminService::load(
            Arc::clone(&database),
            state.bootstrap_shared(),
            state.clock_shared(),
            state.ids_shared(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("administration startup failed"))?,
    );
    let runtime_settings = administration
        .runtime_settings()
        .await
        .map_err(|_| anyhow::anyhow!("runtime settings startup failed"))?;
    let adapters: Arc<dyn AdapterProvider> = Arc::new(administration.adapters());
    let scheduler = Arc::new(
        Scheduler::new(
            Arc::clone(&database),
            scheduler_service,
            adapters,
            state.clock_shared(),
            format!("process-{}", state.ids().next_id().simple()),
            SchedulerConfig {
                poll_interval: std::time::Duration::from_secs(runtime_setting(
                    &runtime_settings,
                    "scheduler.poll_interval_seconds",
                )?),
                lease_duration: std::time::Duration::from_secs(runtime_setting(
                    &runtime_settings,
                    "scheduler.lease_seconds",
                )?),
                adapter_timeout: std::time::Duration::from_secs(runtime_setting(
                    &runtime_settings,
                    "adapters.timeout_seconds",
                )?),
                adapter_concurrency: usize::try_from(runtime_setting(
                    &runtime_settings,
                    "adapters.max_concurrency",
                )?)
                .context("adapter concurrency is too large")?,
                candidate_batch_size: 200,
            },
        )
        .context("scheduler workload startup failed")?,
    );
    let scheduler_workload = Arc::new(SchedulerWorkload::new(scheduler));
    let workloads = Arc::new(
        WorkloadController::new(
            Arc::clone(&database),
            state.clock_shared(),
            Arc::clone(&mcp),
            Arc::clone(&scheduler_workload),
        )
        .await
        .context("workload controller startup failed")?,
    );
    let backups = Arc::new(
        BackupManager::open(
            Arc::clone(&database),
            state.bootstrap().backup_directory(),
            state.bootstrap().owner_id(),
            state.clock_shared(),
            state.ids_shared(),
        )
        .await
        .context("backup administration startup failed")?,
    );
    let restore = Arc::new(RestoreManager::new(
        Arc::clone(&database),
        Arc::clone(&backups),
        Arc::clone(&workloads),
    ));
    let web_api = WebApiState::new(web_sessions, web_service)
        .with_administration(Arc::clone(&administration), Arc::clone(&workloads))
        .with_backups(Arc::clone(&backups))
        .with_restore(restore);
    let mut state = state
        .with_mcp(Arc::clone(&mcp))
        .with_web_api(web_api)
        .with_workloads(Arc::clone(&workloads));
    if let Some(assets) = webui_assets {
        state = state.with_webui_assets(assets);
    }
    let listener = TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind the fixed listener at {address}"))?;

    state.set_ready(true);
    tracing::info!(event = "control_plane_started", %address);
    let result = run(listener, build_router(state.clone()), shutdown_signal())
        .await
        .context("control plane failed");
    state.set_ready(false);
    workloads
        .shutdown()
        .await
        .context("workload shutdown failed")?;
    drop(state);
    drop(workloads);
    drop(mcp);
    drop(scheduler_workload);
    drop(administration);
    drop(backups);
    Arc::try_unwrap(database)
        .map_err(|_| anyhow::anyhow!("database still has active application references"))?
        .close()
        .await
        .context("database shutdown failed")?;
    result
}

fn runtime_setting(settings: &[RuntimeSetting], key: &str) -> anyhow::Result<u64> {
    let value = settings
        .iter()
        .find(|setting| setting.key == key)
        .with_context(|| format!("required runtime setting `{key}` is missing"))?
        .value;
    u64::try_from(value).with_context(|| format!("runtime setting `{key}` is invalid"))
}
