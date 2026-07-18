use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use remindi::{
    clock::{FixedClock, IdGenerator},
    db::DatabaseManager,
    remindi::{
        Actor, AddRequest, CheckRequest, CompleteRequest, EvidenceInput, EvidenceSource,
        EvidenceType, HistoryRequest, LifecycleEvent, ListRequest, Priority, Readiness,
        RemindiService, RemindiState, ServiceError, SnoozeRequest, Trigger, UpdateRequest,
    },
};
use serde_json::json;
use sqlx::Row;
use time::macros::datetime;
use uuid::Uuid;

#[derive(Default)]
struct SequenceIds(AtomicU64);

impl IdGenerator for SequenceIds {
    fn next_id(&self) -> Uuid {
        Uuid::from_u128(self.0.fetch_add(1, Ordering::Relaxed).into())
    }
}

async fn setup_service(
    path: &std::path::Path,
    owner: &str,
) -> (Arc<DatabaseManager>, RemindiService) {
    let database = Arc::new(DatabaseManager::open(path).await.expect("database opens"));
    let service = RemindiService::new(
        Arc::clone(&database),
        owner,
        b"unit-test-mcp-secret",
        Arc::new(FixedClock::new(datetime!(2026-07-19 06:00 UTC))),
        Arc::new(SequenceIds::default()),
    );
    (database, service)
}

fn actor() -> Actor {
    Actor::agent("agent-a", Some("request-a".into()))
}

fn add(key: &str) -> AddRequest {
    AddRequest {
        project_id: "project-a".into(),
        task_id: Some("task-a".into()),
        message: "Collect evidence".into(),
        instructions: Some("Run the acceptance check".into()),
        priority: Priority::High,
        trigger: Trigger::AtTime {
            at: datetime!(2026-07-19 07:00 UTC),
        },
        recurrence: None,
        overdue_after_seconds: 60,
        links: vec![],
        session_id: Some("session-a".into()),
        task_lineage_id: Some("lineage-a".into()),
        idempotency_key: key.into(),
    }
}

#[tokio::test]
async fn mutation_is_atomic_replayable_owner_scoped_and_durable() {
    let directory = std::env::temp_dir().join(format!("remindi-repository-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&directory).expect("temp directory");
    let path = directory.join("remindi.db");

    let (database, service) = setup_service(&path, "owner-a").await;
    let created = service
        .add(&actor(), add("same-request"))
        .await
        .expect("add");
    let replay = service
        .add(&actor(), add("same-request"))
        .await
        .expect("replay");
    assert_eq!(created.remindi.id, replay.remindi.id);
    assert_eq!(created.remindi.version, 1);

    let different = AddRequest {
        message: "Different validated request".into(),
        ..add("same-request")
    };
    assert!(matches!(
        service.add(&actor(), different).await,
        Err(ServiceError::IdempotencyKeyReused)
    ));

    let history = service
        .history(
            &actor(),
            HistoryRequest {
                remindi_id: created.remindi.id,
                after_sequence: None,
                event_types: vec![],
                limit: 100,
                cursor: None,
            },
        )
        .await
        .expect("history");
    assert_eq!(history.items.len(), 1);
    assert_eq!(history.items[0].event_type.to_string(), "created");

    let other_owner = RemindiService::new(
        Arc::clone(&database),
        "owner-b",
        b"unit-test-mcp-secret",
        Arc::new(FixedClock::new(datetime!(2026-07-19 06:00 UTC))),
        Arc::new(SequenceIds::default()),
    );
    assert!(matches!(
        other_owner
            .history(
                &actor(),
                HistoryRequest {
                    remindi_id: created.remindi.id,
                    after_sequence: None,
                    event_types: vec![],
                    limit: 100,
                    cursor: None,
                },
            )
            .await,
        Err(ServiceError::NotFound)
    ));
    drop(other_owner);
    drop(service);
    Arc::try_unwrap(database)
        .expect("sole manager")
        .close()
        .await
        .expect("close");

    let (database, restarted) = setup_service(&path, "owner-a").await;
    let listed = restarted
        .list(&actor(), ListRequest::default())
        .await
        .expect("list after restart");
    assert_eq!(listed.items.len(), 1);
    assert_eq!(listed.items[0].id, created.remindi.id);
    drop(restarted);
    Arc::try_unwrap(database)
        .expect("sole manager")
        .close()
        .await
        .expect("close");
}

#[tokio::test]
async fn complete_commits_item_evidence_event_and_idempotency_together() {
    let directory = std::env::temp_dir().join(format!("remindi-complete-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&directory).expect("temp directory");
    let path = directory.join("remindi.db");
    let (database, service) = setup_service(&path, "owner-a").await;
    let created = service.add(&actor(), add("create-key")).await.expect("add");

    let completed = service
        .complete(
            &actor(),
            CompleteRequest {
                remindi_id: created.remindi.id,
                expected_version: 1,
                evidence: EvidenceInput {
                    evidence_type: EvidenceType::TestResult,
                    summary: "Acceptance check passed".into(),
                    reference_uri: Some("https://example.invalid/run/1".into()),
                    content_hash: None,
                    observed_at: datetime!(2026-07-19 06:00 UTC),
                    metadata: Some(json!({"suite": "acceptance"})),
                    source: EvidenceSource::AuthenticatedActor,
                },
                completion_note: Some("Verified".into()),
                idempotency_key: "complete-key".into(),
            },
            Duration::from_secs(30),
        )
        .await
        .expect("complete");
    assert_eq!(completed.remindi.version, 2);

    let history = service
        .history(
            &actor(),
            HistoryRequest {
                remindi_id: created.remindi.id,
                after_sequence: None,
                event_types: vec![],
                limit: 100,
                cursor: None,
            },
        )
        .await
        .expect("history");
    assert_eq!(history.items.len(), 2);
    assert!(history.evidence.is_some());

    let stale = service
        .complete(
            &actor(),
            CompleteRequest {
                remindi_id: created.remindi.id,
                expected_version: 1,
                evidence: EvidenceInput {
                    evidence_type: EvidenceType::TestResult,
                    summary: "Still passed".into(),
                    reference_uri: Some("https://example.invalid/run/2".into()),
                    content_hash: None,
                    observed_at: datetime!(2026-07-19 06:00 UTC),
                    metadata: None,
                    source: EvidenceSource::AuthenticatedActor,
                },
                completion_note: None,
                idempotency_key: "another-key".into(),
            },
            Duration::from_secs(30),
        )
        .await;
    assert!(matches!(
        stale,
        Err(ServiceError::VersionConflict { current_version: 2 })
    ));

    drop(service);
    Arc::try_unwrap(database)
        .expect("sole manager")
        .close()
        .await
        .expect("close");
}

#[tokio::test]
async fn check_snooze_and_update_use_cas_and_append_ordered_history() {
    let directory = std::env::temp_dir().join(format!("remindi-lifecycle-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&directory).expect("temp directory");
    let path = directory.join("remindi.db");
    let (database, service) = setup_service(&path, "owner-a").await;
    let created = service
        .add(
            &actor(),
            AddRequest {
                trigger: Trigger::AtTime {
                    at: datetime!(2026-07-19 05:00 UTC),
                },
                overdue_after_seconds: 0,
                ..add("lifecycle-create")
            },
        )
        .await
        .expect("add");

    let check_request = || CheckRequest {
        project_id: "project-a".into(),
        task_id: Some("task-a".into()),
        session_id: Some("   ".into()),
        task_lineage_id: None,
        lifecycle_event: LifecycleEvent::Checkpoint,
        active_goal_ids: vec![],
        include_scheduled: false,
        limit: 50,
        cursor: None,
    };
    let due = service.check(&actor(), check_request()).await.expect("due");
    assert_eq!(due.items.len(), 1);
    assert_eq!(due.items[0].remindi.state, RemindiState::Due);
    let overdue = service
        .check(&actor(), check_request())
        .await
        .expect("overdue");
    assert_eq!(overdue.items[0].remindi.state, RemindiState::Overdue);

    let snoozed = service
        .snooze(
            &actor(),
            SnoozeRequest {
                remindi_id: created.remindi.id,
                expected_version: 3,
                snooze_until: datetime!(2026-07-19 07:00 UTC),
                reason: "Wait for another sample".into(),
                idempotency_key: "lifecycle-snooze".into(),
            },
            Duration::from_secs(7200),
        )
        .await
        .expect("snooze");
    assert_eq!(snoozed.remindi.state, RemindiState::Snoozed);
    assert_eq!(snoozed.remindi.version, 4);

    let updated = service
        .update(
            &actor(),
            UpdateRequest {
                remindi_id: created.remindi.id,
                expected_version: 4,
                message: Some("Collect more evidence".into()),
                instructions: None,
                priority: None,
                trigger: None,
                recurrence: None,
                overdue_after_seconds: None,
                links: None,
                occurrence_disposition: None,
                reason: "Clarify required work".into(),
                idempotency_key: "lifecycle-update".into(),
            },
        )
        .await
        .expect("update");
    assert_eq!(updated.remindi.version, 5);
    assert_eq!(updated.remindi.message, "Collect more evidence");

    let history = service
        .history(
            &actor(),
            HistoryRequest {
                remindi_id: created.remindi.id,
                after_sequence: None,
                event_types: vec![],
                limit: 100,
                cursor: None,
            },
        )
        .await
        .expect("history");
    let sequences: Vec<_> = history
        .items
        .iter()
        .map(|event| event.sequence.expect("persisted sequence"))
        .collect();
    assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(history.items.len(), 5);

    drop(service);
    Arc::try_unwrap(database)
        .expect("sole manager")
        .close()
        .await
        .expect("close");
}

#[tokio::test]
async fn check_returns_manual_verification_without_include_scheduled() {
    let directory = std::env::temp_dir().join(format!("remindi-manual-check-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&directory).expect("temp directory");
    let path = directory.join("remindi.db");
    let (database, service) = setup_service(&path, "owner-a").await;
    service
        .add(
            &actor(),
            AddRequest {
                trigger: Trigger::Condition {
                    adapter: "http_health".into(),
                    parameters: json!({"target": "service-api"}),
                    poll_interval_seconds: Some(300),
                    manual_check_at: Some(datetime!(2026-07-19 06:00 UTC)),
                },
                ..add("manual-create")
            },
        )
        .await
        .expect("condition item added");

    let checked = service
        .check(
            &actor(),
            CheckRequest {
                project_id: "project-a".into(),
                task_id: Some("task-a".into()),
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
        .expect("manual fallback checked");

    assert_eq!(checked.items.len(), 1);
    assert_eq!(checked.items[0].readiness, Readiness::ManualVerification);

    drop(service);
    Arc::try_unwrap(database)
        .expect("sole manager")
        .close()
        .await
        .expect("close");
}

#[tokio::test]
async fn list_uses_authenticated_keyset_cursor_and_no_offset_query_plan() {
    let directory = std::env::temp_dir().join(format!("remindi-list-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&directory).expect("temp directory");
    let path = directory.join("remindi.db");
    let (database, service) = setup_service(&path, "owner-a").await;
    service.add(&actor(), add("create-one")).await.expect("one");
    service.add(&actor(), add("create-two")).await.expect("two");

    let first = service
        .list(
            &actor(),
            ListRequest {
                limit: 1,
                ..ListRequest::default()
            },
        )
        .await
        .expect("first");
    assert_eq!(first.items.len(), 1);
    let cursor = first.next_cursor.expect("next cursor");
    let second = service
        .list(
            &actor(),
            ListRequest {
                limit: 1,
                cursor: Some(cursor.clone()),
                ..ListRequest::default()
            },
        )
        .await
        .expect("second");
    assert_eq!(second.items.len(), 1);
    assert_ne!(first.items[0].id, second.items[0].id);

    let mut tampered = cursor;
    tampered.push('A');
    assert!(matches!(
        service
            .list(
                &actor(),
                ListRequest {
                    limit: 1,
                    cursor: Some(tampered),
                    ..ListRequest::default()
                },
            )
            .await,
        Err(ServiceError::InvalidCursor)
    ));

    let mut connection = database.connection().await.expect("connection");
    let plan = sqlx::query(
        "EXPLAIN QUERY PLAN
         SELECT * FROM remindi WHERE owner_id = 'owner-a'
         AND (created_at < '9999' OR (created_at = '9999' AND id > '0'))
         ORDER BY created_at DESC, id ASC LIMIT 2",
    )
    .fetch_all(connection.as_mut())
    .await
    .expect("query plan");
    let detail = plan
        .iter()
        .map(|row| row.get::<String, _>("detail"))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(!detail.to_ascii_uppercase().contains("OFFSET"));
}
