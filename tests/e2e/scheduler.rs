use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use remindi::{
    clock::{Clock, IdGenerator},
    db::DatabaseManager,
    remindi::{
        Actor, AddRequest, LifecycleEvent, ListRequest, Priority, RemindiService, RemindiState,
        SnoozeRequest, Trigger,
    },
    scheduler::{AdapterProvider, LeaseError, RunExit, Scheduler, SchedulerConfig, SchedulerLease},
    triggers::adapters::{AdapterMetadata, AdapterResult, AdapterStatus, ConditionAdapter},
};
use schemars::Schema;
use serde_json::{Value, json};
use time::{OffsetDateTime, macros::datetime};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Default)]
struct SequenceIds(AtomicU64);

impl IdGenerator for SequenceIds {
    fn next_id(&self) -> Uuid {
        Uuid::from_u128(u128::from(self.0.fetch_add(1, Ordering::Relaxed) + 1))
    }
}

struct TestClock(Mutex<OffsetDateTime>);

impl TestClock {
    fn new(now: OffsetDateTime) -> Self {
        Self(Mutex::new(now))
    }

    fn set(&self, now: OffsetDateTime) {
        *self.0.lock().expect("clock lock") = now;
    }
}

impl Clock for TestClock {
    fn now(&self) -> OffsetDateTime {
        *self.0.lock().expect("clock lock")
    }
}

struct ProbeAdapter {
    database: Arc<DatabaseManager>,
    status: AdapterStatus,
}

#[async_trait]
impl ConditionAdapter for ProbeAdapter {
    fn name(&self) -> &'static str {
        "probe"
    }

    fn parameter_schema(&self) -> Schema {
        schemars::schema_for!(Value)
    }

    async fn evaluate(
        &self,
        _params: Value,
        _deadline: Instant,
        _cancel: CancellationToken,
    ) -> AdapterResult {
        let transaction = self
            .database
            .begin_immediate()
            .await
            .expect("adapter I/O is outside scheduler write transactions");
        transaction.rollback().await.expect("probe rollback");
        AdapterResult {
            status: self.status,
            observed_at: datetime!(2026-07-19 06:00:31 UTC),
            summary: format!("probe returned {:?}", self.status),
            metadata: AdapterMetadata {
                adapter_version: "test",
                latency_ms: 0,
            },
        }
    }
}

struct TestAdapters(BTreeMap<String, Arc<dyn ConditionAdapter>>);

impl AdapterProvider for TestAdapters {
    fn get(&self, name: &str) -> Option<Arc<dyn ConditionAdapter>> {
        self.0.get(name).cloned()
    }
}

fn config() -> SchedulerConfig {
    SchedulerConfig {
        poll_interval: Duration::from_millis(20),
        lease_duration: Duration::from_millis(90),
        adapter_timeout: Duration::from_secs(1),
        adapter_concurrency: 2,
        candidate_batch_size: 50,
    }
}

fn actor() -> Actor {
    Actor::agent("test-agent", None)
}

fn add(trigger: Trigger, key: &str) -> AddRequest {
    AddRequest {
        project_id: "project-a".into(),
        task_id: Some("task-a".into()),
        message: format!("candidate {key}"),
        instructions: None,
        priority: Priority::Normal,
        trigger,
        recurrence: None,
        overdue_after_seconds: 60,
        links: vec![],
        session_id: None,
        task_lineage_id: None,
        idempotency_key: format!("scheduler-{key}"),
    }
}

async fn setup(
    name: &str,
) -> (
    std::path::PathBuf,
    Arc<DatabaseManager>,
    Arc<RemindiService>,
    Arc<TestClock>,
) {
    let directory =
        std::env::temp_dir().join(format!("remindi-scheduler-{name}-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&directory).expect("temp directory");
    let database = Arc::new(
        DatabaseManager::open(directory.join("remindi.db"))
            .await
            .expect("database"),
    );
    let clock = Arc::new(TestClock::new(datetime!(2026-07-19 06:00 UTC)));
    let service = Arc::new(RemindiService::new(
        Arc::clone(&database),
        "owner-a",
        b"scheduler-test-secret",
        clock.clone(),
        Arc::new(SequenceIds::default()),
    ));
    (directory, database, service, clock)
}

#[tokio::test]
async fn lease_excludes_a_second_loop_and_recovers_after_expiry() {
    let (directory, database, service, clock) = setup("lease").await;
    let duration = time::Duration::seconds(90);
    let first = SchedulerLease::new(Arc::clone(&database), "first", duration).expect("lease");
    let second = SchedulerLease::new(Arc::clone(&database), "second", duration).expect("lease");

    let mut first_guard = first.acquire(clock.now()).await.expect("first acquires");
    assert!(matches!(
        second.acquire(clock.now()).await,
        Err(LeaseError::AlreadyHeld)
    ));
    let duplicate_holder =
        SchedulerLease::new(Arc::clone(&database), "first", duration).expect("duplicate lease");
    assert!(matches!(
        duplicate_holder.acquire(clock.now()).await,
        Err(LeaseError::AlreadyHeld)
    ));

    clock.set(datetime!(2026-07-19 06:01:31 UTC));
    let second_guard = second.acquire(clock.now()).await.expect("expired takeover");
    assert!(matches!(
        first.renew(&mut first_guard, clock.now()).await,
        Err(LeaseError::Lost)
    ));
    second.release(&second_guard).await.expect("clean release");
    let replacement = first
        .acquire(clock.now())
        .await
        .expect("released reacquire");
    first.release(&replacement).await.expect("release");

    drop(service);
    drop(first);
    drop(second);
    drop(duplicate_holder);
    Arc::try_unwrap(database)
        .expect("database owner")
        .close()
        .await
        .expect("close");
    std::fs::remove_dir_all(directory).expect("cleanup");
}

#[tokio::test]
async fn poll_is_deterministic_and_isolates_adapter_failures_outside_transactions() {
    let (directory, database, service, clock) = setup("poll").await;
    service
        .add(
            &actor(),
            add(
                Trigger::AtTime {
                    at: datetime!(2026-07-19 05:59 UTC),
                },
                "time",
            ),
        )
        .await
        .expect("time candidate");
    service
        .add(
            &actor(),
            add(
                Trigger::Condition {
                    adapter: "probe".into(),
                    parameters: json!({}),
                    poll_interval_seconds: Some(30),
                    manual_check_at: None,
                },
                "condition-satisfied",
            ),
        )
        .await
        .expect("condition candidate");
    service
        .add(
            &actor(),
            add(
                Trigger::Condition {
                    adapter: "failing_probe".into(),
                    parameters: json!({}),
                    poll_interval_seconds: Some(30),
                    manual_check_at: None,
                },
                "condition-error",
            ),
        )
        .await
        .expect("error candidate");
    service
        .add(
            &actor(),
            add(
                Trigger::Condition {
                    adapter: "unavailable".into(),
                    parameters: json!({}),
                    poll_interval_seconds: Some(30),
                    manual_check_at: Some(datetime!(2026-07-19 06:00:15 UTC)),
                },
                "manual-fallback",
            ),
        )
        .await
        .expect("manual candidate");
    clock.set(datetime!(2026-07-19 06:00:31 UTC));

    let adapters = Arc::new(TestAdapters(BTreeMap::from([
        (
            "probe".to_owned(),
            Arc::new(ProbeAdapter {
                database: Arc::clone(&database),
                status: AdapterStatus::Satisfied,
            }) as Arc<dyn ConditionAdapter>,
        ),
        (
            "failing_probe".to_owned(),
            Arc::new(ProbeAdapter {
                database: Arc::clone(&database),
                status: AdapterStatus::Error,
            }) as Arc<dyn ConditionAdapter>,
        ),
    ])));
    let scheduler = Scheduler::new(
        Arc::clone(&database),
        Arc::clone(&service),
        adapters,
        clock.clone(),
        "poller",
        SchedulerConfig {
            lease_duration: Duration::from_secs(900),
            ..config()
        },
    )
    .expect("scheduler");
    let mut guard = scheduler.acquire().await.expect("lease");
    let report = scheduler
        .poll_once(&mut guard, CancellationToken::new())
        .await
        .expect("poll");
    assert_eq!(report.selected, 4);
    assert_eq!(report.applied, 4);
    assert_eq!(report.failures, 0);

    let items = service
        .list(&actor(), ListRequest::default())
        .await
        .expect("list")
        .items;
    assert_eq!(
        items
            .iter()
            .find(|item| item.message == "candidate time")
            .expect("time item")
            .state,
        RemindiState::Due
    );
    assert_eq!(
        items
            .iter()
            .find(|item| item.message == "candidate condition-satisfied")
            .expect("satisfied item")
            .state,
        RemindiState::Due
    );
    let failed = items
        .iter()
        .find(|item| item.message == "candidate condition-error")
        .expect("failed item");
    assert_eq!(failed.state, RemindiState::Scheduled);
    assert_eq!(
        failed
            .last_condition_status
            .map(|status| status.to_string()),
        Some("error".to_owned())
    );
    assert_eq!(
        failed.next_evaluation_at,
        Some(datetime!(2026-07-19 06:01:01 UTC))
    );
    assert_eq!(
        items
            .iter()
            .find(|item| item.message == "candidate manual-fallback")
            .expect("manual fallback")
            .state,
        RemindiState::Due
    );

    let time_item = items
        .iter()
        .find(|item| item.message == "candidate time")
        .expect("time item");
    service
        .snooze(
            &actor(),
            SnoozeRequest {
                remindi_id: time_item.id,
                expected_version: time_item.version,
                snooze_until: datetime!(2026-07-19 06:00:40 UTC),
                reason: "bounded scheduler test".into(),
                idempotency_key: "scheduler-snooze".into(),
            },
            Duration::from_secs(31_536_000),
        )
        .await
        .expect("snooze");
    clock.set(datetime!(2026-07-19 06:00:41 UTC));
    let expiry = scheduler
        .poll_once(&mut guard, CancellationToken::new())
        .await
        .expect("snooze expiry poll");
    assert_eq!(expiry.selected, 1);
    let time_item = service
        .list(&actor(), ListRequest::default())
        .await
        .expect("list after expiry")
        .items
        .into_iter()
        .find(|item| item.message == "candidate time")
        .expect("time item after expiry");
    assert_eq!(time_item.state, RemindiState::Due);

    clock.set(datetime!(2026-07-19 06:01:32 UTC));
    scheduler
        .poll_once(&mut guard, CancellationToken::new())
        .await
        .expect("overdue poll");
    let time_item = service
        .list(&actor(), ListRequest::default())
        .await
        .expect("list after overdue")
        .items
        .into_iter()
        .find(|item| item.message == "candidate time")
        .expect("time item after overdue");
    assert_eq!(time_item.state, RemindiState::Overdue);

    scheduler.release(&guard).await.expect("release");
    drop(scheduler);
    drop(service);
    Arc::try_unwrap(database)
        .expect("database owner")
        .close()
        .await
        .expect("close");
    std::fs::remove_dir_all(directory).expect("cleanup");
}

#[tokio::test]
async fn desired_stop_and_cancellation_leave_no_lease_and_do_not_break_pull_checks() {
    let (directory, database, service, clock) = setup("lifecycle").await;
    let scheduler = Arc::new(
        Scheduler::new(
            Arc::clone(&database),
            Arc::clone(&service),
            Arc::new(TestAdapters(BTreeMap::new())),
            clock,
            "lifecycle",
            config(),
        )
        .expect("scheduler"),
    );
    {
        let mut connection = database.connection().await.expect("connection");
        sqlx::query(
            "UPDATE service_runtime SET desired_state = 'stopped' WHERE component = 'scheduler'",
        )
        .execute(connection.as_mut())
        .await
        .expect("persist stop");
    }
    assert_eq!(
        scheduler
            .run(CancellationToken::new())
            .await
            .expect("stopped"),
        RunExit::DesiredStopped
    );

    let pull = service
        .check(
            &actor(),
            remindi::remindi::CheckRequest {
                project_id: "project-a".into(),
                task_id: None,
                session_id: Some("session-b".into()),
                task_lineage_id: None,
                lifecycle_event: LifecycleEvent::Checkpoint,
                active_goal_ids: vec![],
                include_scheduled: false,
                limit: 50,
                cursor: None,
            },
        )
        .await
        .expect("pull remains available");
    assert!(pull.items.is_empty());

    {
        let mut connection = database.connection().await.expect("connection");
        sqlx::query(
            "UPDATE service_runtime SET desired_state = 'running' WHERE component = 'scheduler'",
        )
        .execute(connection.as_mut())
        .await
        .expect("persist start");
    }
    let cancel = CancellationToken::new();
    let task = {
        let scheduler = Arc::clone(&scheduler);
        let token = cancel.clone();
        tokio::spawn(async move { scheduler.run(token).await })
    };
    tokio::time::sleep(Duration::from_millis(30)).await;
    cancel.cancel();
    assert_eq!(task.await.expect("join").expect("run"), RunExit::Cancelled);
    let mut connection = database.connection().await.expect("connection");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scheduler_leases")
        .fetch_one(connection.as_mut())
        .await
        .expect("lease count");
    assert_eq!(count, 0);
    drop(connection);

    drop(scheduler);
    drop(service);
    Arc::try_unwrap(database)
        .expect("database owner")
        .close()
        .await
        .expect("close");
    std::fs::remove_dir_all(directory).expect("cleanup");
}
