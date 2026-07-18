use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use remindi::{
    clock::{FixedClock, IdGenerator},
    db::DatabaseManager,
    remindi::{
        Actor, AddRequest, CancelRequest, CheckRequest, LifecycleEvent, Priority, RemindiService,
        ServiceError, Trigger,
    },
};
use time::macros::datetime;
use uuid::Uuid;

#[derive(Default)]
struct SequenceIds(AtomicU64);

impl IdGenerator for SequenceIds {
    fn next_id(&self) -> Uuid {
        Uuid::from_u128(self.0.fetch_add(1, Ordering::Relaxed).into())
    }
}

#[tokio::test]
async fn racing_expected_versions_allow_exactly_one_writer_while_readers_continue() {
    let directory = std::env::temp_dir().join(format!("remindi-race-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&directory).expect("temp directory");
    let database = Arc::new(
        DatabaseManager::open(directory.join("remindi.db"))
            .await
            .expect("database"),
    );
    let service = Arc::new(RemindiService::new(
        Arc::clone(&database),
        "owner-a",
        b"unit-test-mcp-secret",
        Arc::new(FixedClock::new(datetime!(2026-07-19 06:00 UTC))),
        Arc::new(SequenceIds::default()),
    ));
    let actor = Actor::agent("agent", None);
    let created = service
        .add(
            &actor,
            AddRequest {
                project_id: "project".into(),
                task_id: None,
                message: "Race this item".into(),
                instructions: None,
                priority: Priority::Normal,
                trigger: Trigger::AtTime {
                    at: datetime!(2026-07-20 06:00 UTC),
                },
                recurrence: None,
                overdue_after_seconds: 0,
                links: vec![],
                session_id: None,
                task_lineage_id: None,
                idempotency_key: "race-create".into(),
            },
        )
        .await
        .expect("add");

    let mut tasks = Vec::new();
    for index in 0..8 {
        let service = Arc::clone(&service);
        let actor = actor.clone();
        let id = created.remindi.id;
        tasks.push(tokio::spawn(async move {
            service
                .cancel(
                    &actor,
                    CancelRequest {
                        remindi_id: id,
                        expected_version: 1,
                        reason: format!("racer {index}"),
                        idempotency_key: format!("cancel-race-{index}"),
                    },
                )
                .await
        }));
    }

    let mut successes = 0;
    let mut conflicts = 0;
    for task in tasks {
        match task.await.expect("task joins") {
            Ok(_) => successes += 1,
            Err(ServiceError::VersionConflict { .. } | ServiceError::InvalidState) => {
                conflicts += 1;
            }
            Err(error) => panic!("unexpected error: {error}"),
        }
    }
    assert_eq!(successes, 1);
    assert_eq!(conflicts, 7);

    let mut readers = Vec::new();
    for _ in 0..8 {
        let service = Arc::clone(&service);
        let actor = actor.clone();
        readers.push(tokio::spawn(async move {
            service
                .list(&actor, Default::default())
                .await
                .expect("reader")
                .items
                .len()
        }));
    }
    for reader in readers {
        assert_eq!(reader.await.expect("reader joins"), 1);
    }
}

#[tokio::test]
async fn concurrent_batched_checks_commit_each_transition_and_event_exactly_once() {
    let directory = std::env::temp_dir().join(format!("remindi-check-race-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&directory).expect("temp directory");
    let database = Arc::new(
        DatabaseManager::open(directory.join("remindi.db"))
            .await
            .expect("database"),
    );
    let service = Arc::new(RemindiService::new(
        Arc::clone(&database),
        "owner-a",
        b"unit-test-mcp-secret",
        Arc::new(FixedClock::new(datetime!(2026-07-19 06:00 UTC))),
        Arc::new(SequenceIds::default()),
    ));
    let actor = Actor::agent("agent", None);
    for index in 0..20 {
        service
            .add(
                &actor,
                AddRequest {
                    project_id: "project".into(),
                    task_id: Some("task".into()),
                    message: format!("Batched transition {index}"),
                    instructions: None,
                    priority: Priority::Normal,
                    trigger: Trigger::AtTime {
                        at: datetime!(2026-07-19 05:00 UTC),
                    },
                    recurrence: None,
                    overdue_after_seconds: 3_600,
                    links: vec![],
                    session_id: None,
                    task_lineage_id: None,
                    idempotency_key: format!("check-race-create-{index}"),
                },
            )
            .await
            .expect("item added");
    }

    let mut checks = Vec::new();
    for _ in 0..2 {
        let service = Arc::clone(&service);
        let actor = actor.clone();
        checks.push(tokio::spawn(async move {
            service
                .check(
                    &actor,
                    CheckRequest {
                        project_id: "project".into(),
                        task_id: Some("task".into()),
                        session_id: None,
                        task_lineage_id: None,
                        lifecycle_event: LifecycleEvent::Checkpoint,
                        active_goal_ids: vec![],
                        include_scheduled: false,
                        limit: 50,
                        cursor: None,
                    },
                )
                .await
                .expect("concurrent check")
                .items
                .len()
        }));
    }
    let first = checks.remove(0).await.expect("first check joins");
    let second = checks.remove(0).await.expect("second check joins");
    assert!(matches!((first, second), (20, 20) | (20, 0) | (0, 20)));

    let mut connection = database.connection().await.expect("connection");
    let transitioned: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM remindi
         WHERE owner_id = 'owner-a' AND state = 'due' AND version = 2",
    )
    .fetch_one(connection.as_mut())
    .await
    .expect("transitioned count");
    let events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM remindi_events WHERE event_type = 'became_due'")
            .fetch_one(connection.as_mut())
            .await
            .expect("transition event count");
    assert_eq!(transitioned, 20);
    assert_eq!(events, 20);
    drop(connection);
    drop(service);
    Arc::try_unwrap(database)
        .expect("sole database owner")
        .close()
        .await
        .expect("database closes");
}

#[tokio::test]
async fn concurrent_identical_idempotency_retries_return_one_original_result() {
    let directory = std::env::temp_dir().join(format!("remindi-retry-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&directory).expect("temp directory");
    let database = Arc::new(
        DatabaseManager::open(directory.join("remindi.db"))
            .await
            .expect("database"),
    );
    let service = Arc::new(RemindiService::new(
        database,
        "owner-a",
        b"unit-test-mcp-secret",
        Arc::new(FixedClock::new(datetime!(2026-07-19 06:00 UTC))),
        Arc::new(SequenceIds::default()),
    ));
    let request = AddRequest {
        project_id: "project".into(),
        task_id: None,
        message: "Retry this item".into(),
        instructions: None,
        priority: Priority::Normal,
        trigger: Trigger::AtTime {
            at: datetime!(2026-07-20 06:00 UTC),
        },
        recurrence: None,
        overdue_after_seconds: 0,
        links: vec![],
        session_id: None,
        task_lineage_id: None,
        idempotency_key: "same-racing-key".into(),
    };
    let actor = Actor::agent("agent", None);
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let service = Arc::clone(&service);
        let request = request.clone();
        let actor = actor.clone();
        tasks.push(tokio::spawn(async move {
            service.add(&actor, request).await.expect("retry succeeds")
        }));
    }
    let mut ids = Vec::new();
    for task in tasks {
        ids.push(task.await.expect("retry joins").remindi.id);
    }
    assert!(ids.iter().all(|id| *id == ids[0]));
}
