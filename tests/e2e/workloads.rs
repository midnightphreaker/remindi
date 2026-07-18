use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use remindi::{
    admin::workloads::{
        ActualState, WorkloadAction, WorkloadComponent, WorkloadController, WorkloadError,
        WorkloadRuntime,
    },
    app::AppState,
    clock::{FixedClock, UuidV7Generator},
    config::BootstrapConfig,
    db::DatabaseManager,
    http::router::build_router,
    mcp::server::McpWorkload,
    remindi::RemindiService,
    scheduler::{AdapterProvider, Scheduler, SchedulerConfig, SchedulerWorkload},
    triggers::adapters::AdapterRegistry,
};
use secrecy::ExposeSecret;
use time::macros::datetime;
use tokio::sync::Notify;
use tower::ServiceExt;
use uuid::Uuid;

struct ProbeRuntime {
    name: &'static str,
    running: AtomicBool,
    events: Arc<Mutex<Vec<String>>>,
    database: Arc<DatabaseManager>,
    block_stop: AtomicBool,
    stop_entered: Notify,
    release_stop: Notify,
    fail_stop: AtomicBool,
}

impl ProbeRuntime {
    fn new(
        name: &'static str,
        running: bool,
        events: Arc<Mutex<Vec<String>>>,
        database: Arc<DatabaseManager>,
    ) -> Self {
        Self {
            name,
            running: AtomicBool::new(running),
            events,
            database,
            block_stop: AtomicBool::new(false),
            stop_entered: Notify::new(),
            release_stop: Notify::new(),
            fail_stop: AtomicBool::new(false),
        }
    }

    async fn record(&self, action: &str) {
        let mcp = desired(&self.database, "mcp").await;
        let scheduler = desired(&self.database, "scheduler").await;
        self.events.lock().expect("events").push(format!(
            "{action}:{}:mcp={mcp}:scheduler={scheduler}",
            self.name
        ));
    }
}

#[async_trait]
impl WorkloadRuntime for ProbeRuntime {
    fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    async fn start(&self) -> Result<(), String> {
        self.record("start").await;
        self.running.store(true, Ordering::Release);
        Ok(())
    }

    async fn stop(&self) -> Result<(), String> {
        self.record("stop").await;
        if self.block_stop.load(Ordering::Acquire) {
            self.stop_entered.notify_one();
            self.release_stop.notified().await;
        }
        if self.fail_stop.load(Ordering::Acquire) {
            return Err("x".repeat(2_000));
        }
        self.running.store(false, Ordering::Release);
        Ok(())
    }
}

async fn database(name: &str) -> (std::path::PathBuf, Arc<DatabaseManager>) {
    let directory =
        std::env::temp_dir().join(format!("remindi-workloads-{name}-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&directory).expect("temporary directory");
    let database = Arc::new(
        DatabaseManager::open(directory.join("remindi.db"))
            .await
            .expect("database"),
    );
    (directory, database)
}

async fn desired(database: &DatabaseManager, component: &str) -> String {
    let mut connection = database.connection().await.expect("connection");
    sqlx::query_scalar("SELECT desired_state FROM service_runtime WHERE component = ?")
        .bind(component)
        .fetch_one(connection.as_mut())
        .await
        .expect("desired state")
}

async fn cleanup(directory: std::path::PathBuf, database: Arc<DatabaseManager>) {
    Arc::try_unwrap(database)
        .expect("database owner")
        .close()
        .await
        .expect("close");
    std::fs::remove_dir_all(directory).expect("cleanup");
}

async fn lease_count(database: &DatabaseManager) -> i64 {
    let mut connection = database.connection().await.expect("connection");
    sqlx::query_scalar("SELECT COUNT(*) FROM scheduler_leases")
        .fetch_one(connection.as_mut())
        .await
        .expect("lease count")
}

#[tokio::test]
async fn all_persists_atomically_then_transitions_in_stable_order() {
    let (directory, database) = database("all").await;
    let events = Arc::new(Mutex::new(Vec::new()));
    let mcp = Arc::new(ProbeRuntime::new(
        "mcp",
        false,
        Arc::clone(&events),
        Arc::clone(&database),
    ));
    let scheduler = Arc::new(ProbeRuntime::new(
        "scheduler",
        false,
        Arc::clone(&events),
        Arc::clone(&database),
    ));
    let controller = WorkloadController::from_runtimes(
        Arc::clone(&database),
        Arc::new(FixedClock::new(datetime!(2026-07-19 06:00 UTC))),
        Arc::clone(&mcp) as Arc<dyn WorkloadRuntime>,
        Arc::clone(&scheduler) as Arc<dyn WorkloadRuntime>,
    )
    .await
    .expect("restore desired running state");
    assert_eq!(
        events.lock().expect("events").as_slice(),
        [
            "start:mcp:mcp=running:scheduler=running",
            "start:scheduler:mcp=running:scheduler=running"
        ]
    );

    events.lock().expect("events").clear();
    let statuses = controller
        .transition(
            WorkloadComponent::All,
            WorkloadAction::Stop,
            "admin-a",
            Some("req-a"),
        )
        .await
        .expect("all stops");
    assert_eq!(desired(&database, "mcp").await, "stopped");
    assert_eq!(desired(&database, "scheduler").await, "stopped");
    assert_eq!(
        events.lock().expect("events").as_slice(),
        [
            "stop:mcp:mcp=stopped:scheduler=stopped",
            "stop:scheduler:mcp=stopped:scheduler=stopped"
        ]
    );
    assert!(
        statuses
            .iter()
            .all(|status| status.actual == ActualState::Stopped)
    );

    drop(controller);
    drop(mcp);
    drop(scheduler);
    cleanup(directory, database).await;
}

#[tokio::test]
async fn persisted_desired_state_is_restored_without_process_control() {
    let (directory, database) = database("restore").await;
    {
        let mut transaction = database.begin_immediate().await.expect("transaction");
        sqlx::query("UPDATE service_runtime SET desired_state = 'stopped'")
            .execute(transaction.as_mut())
            .await
            .expect("seed stopped");
        transaction.commit().await.expect("commit");
    }
    let events = Arc::new(Mutex::new(Vec::new()));
    let controller = WorkloadController::from_runtimes(
        Arc::clone(&database),
        Arc::new(FixedClock::new(datetime!(2026-07-19 06:00 UTC))),
        Arc::new(ProbeRuntime::new(
            "mcp",
            true,
            Arc::clone(&events),
            Arc::clone(&database),
        )),
        Arc::new(ProbeRuntime::new(
            "scheduler",
            true,
            Arc::clone(&events),
            Arc::clone(&database),
        )),
    )
    .await
    .expect("restore stopped state");
    assert_eq!(
        events.lock().expect("events").as_slice(),
        [
            "stop:mcp:mcp=stopped:scheduler=stopped",
            "stop:scheduler:mcp=stopped:scheduler=stopped"
        ]
    );
    assert!(
        controller
            .status()
            .iter()
            .all(|status| status.actual == ActualState::Stopped)
    );

    drop(controller);
    cleanup(directory, database).await;
}

#[tokio::test]
async fn overlapping_transition_conflicts_and_failure_is_bounded() {
    let (directory, database) = database("conflict").await;
    let events = Arc::new(Mutex::new(Vec::new()));
    let mcp = Arc::new(ProbeRuntime::new(
        "mcp",
        true,
        Arc::clone(&events),
        Arc::clone(&database),
    ));
    let scheduler = Arc::new(ProbeRuntime::new(
        "scheduler",
        true,
        events,
        Arc::clone(&database),
    ));
    let controller = Arc::new(
        WorkloadController::from_runtimes(
            Arc::clone(&database),
            Arc::new(FixedClock::new(datetime!(2026-07-19 06:00 UTC))),
            Arc::clone(&mcp) as Arc<dyn WorkloadRuntime>,
            Arc::clone(&scheduler) as Arc<dyn WorkloadRuntime>,
        )
        .await
        .expect("controller"),
    );

    mcp.block_stop.store(true, Ordering::Release);
    let stopping = {
        let controller = Arc::clone(&controller);
        tokio::spawn(async move {
            controller
                .transition(
                    WorkloadComponent::Mcp,
                    WorkloadAction::Stop,
                    "admin-a",
                    None,
                )
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(1), mcp.stop_entered.notified())
        .await
        .expect("stop entered");
    assert!(matches!(
        controller
            .transition(
                WorkloadComponent::Scheduler,
                WorkloadAction::Stop,
                "admin-a",
                None,
            )
            .await,
        Err(WorkloadError::TransitionConflict)
    ));
    mcp.release_stop.notify_one();
    stopping.await.expect("join").expect("stop succeeds");

    scheduler.fail_stop.store(true, Ordering::Release);
    assert!(matches!(
        controller
            .transition(
                WorkloadComponent::Scheduler,
                WorkloadAction::Stop,
                "admin-a",
                None,
            )
            .await,
        Err(WorkloadError::TransitionFailed { .. })
    ));
    let failed = controller
        .status()
        .into_iter()
        .find(|status| status.component == WorkloadComponent::Scheduler)
        .expect("scheduler status");
    assert_eq!(failed.actual, ActualState::Failed);
    assert!(failed.last_error.expect("last error").len() <= 512);
    assert_eq!(desired(&database, "scheduler").await, "stopped");

    drop(controller);
    drop(mcp);
    drop(scheduler);
    cleanup(directory, database).await;
}

#[tokio::test]
async fn concrete_workloads_stop_without_stopping_control_plane_and_restart_cleanly() {
    let directory = std::env::temp_dir().join(format!("remindi-workloads-real-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&directory).expect("temporary directory");
    let config = Arc::new(
        BootstrapConfig::from_pairs([
            ("REMINDI_OWNER_ID", "owner-a"),
            ("REMINDI_MCP_TOKEN", "workload-test-token"),
            (
                "REMINDI_DB_PATH",
                directory.join("remindi.db").to_str().expect("UTF-8 path"),
            ),
            ("REMINDI_HTTP_ALLOWED_HOSTS", "localhost"),
            ("REMINDI_WEBUI_ENABLE", "false"),
        ])
        .expect("configuration"),
    );
    let database = Arc::new(
        DatabaseManager::open(config.database_path())
            .await
            .expect("database"),
    );
    let clock = Arc::new(FixedClock::new(datetime!(2026-07-19 06:00 UTC)));
    let state = AppState::new(
        Arc::clone(&config),
        clock.clone(),
        Arc::new(UuidV7Generator),
    )
    .with_database(Arc::clone(&database));
    let mcp = Arc::new(McpWorkload::new(&state).expect("MCP"));
    let scheduler_service = Arc::new(RemindiService::new(
        Arc::clone(&database),
        config.owner_id(),
        config.mcp_token().expose_secret().as_bytes(),
        clock.clone(),
        Arc::new(UuidV7Generator),
    ));
    let adapters: Arc<dyn AdapterProvider> = Arc::new(AdapterRegistry::disabled(clock.clone()));
    let scheduler = Arc::new(
        Scheduler::new(
            Arc::clone(&database),
            scheduler_service,
            adapters,
            clock.clone(),
            "workload-test",
            SchedulerConfig {
                poll_interval: Duration::from_secs(10),
                lease_duration: Duration::from_secs(30),
                adapter_timeout: Duration::from_secs(1),
                adapter_concurrency: 1,
                candidate_batch_size: 10,
            },
        )
        .expect("scheduler"),
    );
    let scheduler = Arc::new(SchedulerWorkload::new(scheduler));
    let controller = Arc::new(
        WorkloadController::new(
            Arc::clone(&database),
            clock,
            Arc::clone(&mcp),
            Arc::clone(&scheduler),
        )
        .await
        .expect("controller"),
    );
    assert_eq!(lease_count(&database).await, 1);
    let state = state
        .with_mcp(Arc::clone(&mcp))
        .with_workloads(Arc::clone(&controller));
    state.set_ready(true);
    let router: Router = build_router(state.clone());

    controller
        .transition(
            WorkloadComponent::All,
            WorkloadAction::Stop,
            "admin-a",
            Some("req-stop"),
        )
        .await
        .expect("stop all");
    assert_eq!(lease_count(&database).await, 0);
    let stopped = router
        .clone()
        .oneshot(
            Request::post("/mcp")
                .header("host", "localhost")
                .header("authorization", "Bearer workload-test-token")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(stopped.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(stopped.headers()["retry-after"], "1");
    let live = router
        .clone()
        .oneshot(
            Request::get("/health/live")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("live response");
    assert_eq!(live.status(), StatusCode::OK);
    let ready = router
        .clone()
        .oneshot(
            Request::get("/health/ready")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("ready response");
    assert_eq!(ready.status(), StatusCode::OK);

    controller
        .transition(
            WorkloadComponent::All,
            WorkloadAction::Restart,
            "admin-a",
            Some("req-restart"),
        )
        .await
        .expect("restart all");
    assert!(mcp.is_running());
    assert!(scheduler.is_running());
    assert_eq!(lease_count(&database).await, 1);
    assert_eq!(desired(&database, "mcp").await, "running");
    assert_eq!(desired(&database, "scheduler").await, "running");

    controller.shutdown().await.expect("shutdown");
    assert_eq!(lease_count(&database).await, 0);
    assert_eq!(desired(&database, "mcp").await, "running");
    assert_eq!(desired(&database, "scheduler").await, "running");
    drop(router);
    drop(state);
    drop(controller);
    drop(mcp);
    drop(scheduler);
    cleanup(directory, database).await;
}
