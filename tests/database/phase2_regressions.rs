use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use remindi::{
    clock::{Clock, IdGenerator},
    db::DatabaseManager,
    remindi::{
        Actor, AddRequest, CancelRequest, CheckRequest, CompleteRequest, EvidenceInput,
        EvidenceSource, EvidenceType, HistoryRequest, LifecycleEvent, MissedPolicy,
        OccurrenceDisposition, Priority, RecurrenceSpec, RemindiService, ServiceError, Trigger,
        UpdateRequest,
    },
};
use serde_json::{Value, json};
use sqlx::Row;
use time::{OffsetDateTime, macros::datetime};
use uuid::Uuid;

#[derive(Default)]
struct SequenceIds(AtomicU64);

impl IdGenerator for SequenceIds {
    fn next_id(&self) -> Uuid {
        Uuid::from_u128(u128::from(self.0.fetch_add(1, Ordering::Relaxed) + 1))
    }
}

struct MutableClock(Mutex<OffsetDateTime>);

impl MutableClock {
    fn new(instant: OffsetDateTime) -> Self {
        Self(Mutex::new(instant))
    }

    fn set(&self, instant: OffsetDateTime) {
        *self.0.lock().expect("clock lock") = instant;
    }
}

impl Clock for MutableClock {
    fn now(&self) -> OffsetDateTime {
        *self.0.lock().expect("clock lock")
    }
}

async fn setup() -> (Arc<DatabaseManager>, RemindiService, Arc<MutableClock>) {
    let directory =
        std::env::temp_dir().join(format!("remindi-phase2-regression-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&directory).expect("temp directory");
    let database = Arc::new(
        DatabaseManager::open(directory.join("remindi.db"))
            .await
            .expect("database"),
    );
    let clock = Arc::new(MutableClock::new(datetime!(2026-07-19 06:00 UTC)));
    let service = RemindiService::new(
        Arc::clone(&database),
        "owner-a",
        b"phase2-regression-secret",
        clock.clone(),
        Arc::new(SequenceIds::default()),
    );
    (database, service, clock)
}

fn actor() -> Actor {
    Actor::agent("agent-a", Some("request-a".into()))
}

fn add_request(key: &str, trigger: Trigger) -> AddRequest {
    AddRequest {
        project_id: "project-a".into(),
        task_id: Some("task-a".into()),
        message: "Collect evidence".into(),
        instructions: None,
        priority: Priority::Normal,
        trigger,
        recurrence: None,
        overdue_after_seconds: 60,
        links: vec![],
        session_id: Some("session-a".into()),
        task_lineage_id: Some("lineage-a".into()),
        idempotency_key: key.into(),
    }
}

fn check_request(limit: usize) -> CheckRequest {
    CheckRequest {
        project_id: "project-a".into(),
        task_id: Some("task-a".into()),
        session_id: Some("session-b".into()),
        task_lineage_id: Some("lineage-a".into()),
        lifecycle_event: LifecycleEvent::Checkpoint,
        active_goal_ids: vec![],
        include_scheduled: false,
        limit,
        cursor: None,
    }
}

fn update_request(id: Uuid, version: u64, key: &str) -> UpdateRequest {
    UpdateRequest {
        remindi_id: id,
        expected_version: version,
        message: None,
        instructions: None,
        priority: None,
        trigger: None,
        recurrence: None,
        overdue_after_seconds: None,
        links: None,
        occurrence_disposition: None,
        reason: "Required regression transition".into(),
        idempotency_key: key.into(),
    }
}

#[tokio::test]
async fn check_paginates_in_required_ready_order_across_all_candidates() {
    let (_database, service, _clock) = setup().await;
    service
        .add(
            &actor(),
            add_request(
                "scheduled-first",
                Trigger::AtTime {
                    at: datetime!(2026-07-20 06:00 UTC),
                },
            ),
        )
        .await
        .expect("scheduled item");
    let mut overdue_request = add_request(
        "overdue-second",
        Trigger::AtTime {
            at: datetime!(2026-07-18 06:00 UTC),
        },
    );
    overdue_request.priority = Priority::Critical;
    overdue_request.overdue_after_seconds = 0;
    let overdue = service
        .add(&actor(), overdue_request)
        .await
        .expect("overdue candidate");

    let first_page = service
        .check(&actor(), check_request(1))
        .await
        .expect("check");

    assert_eq!(first_page.items.len(), 1);
    assert_eq!(first_page.items[0].remindi.id, overdue.remindi.id);
}

#[tokio::test]
async fn check_cursor_returns_zero_grace_ready_items_exactly_once_across_pages() {
    let (_database, service, _clock) = setup().await;
    let mut first_request = add_request(
        "page-exactly-once-first",
        Trigger::AtTime {
            at: datetime!(2026-07-18 06:00 UTC),
        },
    );
    first_request.overdue_after_seconds = 0;
    let first = service
        .add(&actor(), first_request)
        .await
        .expect("first ready item");
    let mut second_request = add_request(
        "page-exactly-once-second",
        Trigger::AtTime {
            at: datetime!(2026-07-18 06:00 UTC),
        },
    );
    second_request.overdue_after_seconds = 0;
    let second = service
        .add(&actor(), second_request)
        .await
        .expect("second ready item");

    let first_page = service
        .check(&actor(), check_request(1))
        .await
        .expect("first page");
    assert_eq!(first_page.items.len(), 1);
    let mut second_page_request = check_request(1);
    second_page_request.cursor = first_page.next_cursor;
    let second_page = service
        .check(&actor(), second_page_request)
        .await
        .expect("second page");

    assert_eq!(second_page.items.len(), 1, "the remaining item was lost");
    assert_ne!(
        first_page.items[0].remindi.id, second_page.items[0].remindi.id,
        "an item was returned more than once"
    );
    let mut returned = vec![
        first_page.items[0].remindi.id,
        second_page.items[0].remindi.id,
    ];
    returned.sort_unstable();
    let mut expected = vec![first.remindi.id, second.remindi.id];
    expected.sort_unstable();
    assert_eq!(returned, expected);
}

#[tokio::test]
async fn occurrence_disposition_is_rejected_until_item_is_due_or_overdue() {
    let (_database, service, _clock) = setup().await;
    let mut request = add_request(
        "scheduled-recurrence",
        Trigger::Interval {
            first_at: datetime!(2026-07-20 06:00 UTC),
            every_seconds: 3600,
        },
    );
    request.recurrence = Some(RecurrenceSpec {
        every_seconds: 3600,
        missed_policy: MissedPolicy::CatchUp,
        max_occurrences: None,
        end_at: None,
    });
    let created = service.add(&actor(), request).await.expect("add");
    let mut update = update_request(created.remindi.id, 1, "early-disposition");
    update.occurrence_disposition = Some(OccurrenceDisposition::Acknowledged);

    assert!(matches!(
        service.update(&actor(), update).await,
        Err(ServiceError::InvalidState)
    ));
}

#[tokio::test]
async fn trigger_replacement_validates_the_final_recurrence_pair() {
    let (_database, service, _clock) = setup().await;
    let mut request = add_request(
        "interval-create",
        Trigger::Interval {
            first_at: datetime!(2026-07-19 07:00 UTC),
            every_seconds: 3600,
        },
    );
    request.recurrence = Some(RecurrenceSpec::every_hour());
    let created = service.add(&actor(), request).await.expect("add");
    let mut update = update_request(created.remindi.id, 1, "replace-interval");
    update.trigger = Some(Trigger::AtTime {
        at: datetime!(2026-07-20 06:00 UTC),
    });

    assert!(matches!(
        service.update(&actor(), update).await,
        Err(ServiceError::Validation)
    ));
}

#[tokio::test]
async fn condition_trigger_replacement_sets_the_next_evaluation_anchor() {
    let (_database, service, _clock) = setup().await;
    let created = service
        .add(
            &actor(),
            add_request(
                "condition-anchor-create",
                Trigger::AtTime {
                    at: datetime!(2026-07-20 06:00 UTC),
                },
            ),
        )
        .await
        .expect("add");
    let mut update = update_request(created.remindi.id, 1, "condition-anchor-update");
    update.trigger = Some(Trigger::Condition {
        adapter: "http_health".into(),
        parameters: json!({"target": "service-api", "expected_status": 200}),
        poll_interval_seconds: Some(300),
        manual_check_at: None,
    });

    let updated = service.update(&actor(), update).await.expect("update");

    assert_eq!(
        updated.remindi.next_evaluation_at,
        Some(datetime!(2026-07-19 06:05 UTC))
    );
}

#[tokio::test]
async fn overdue_snooze_expiry_increments_version_once_and_appends_one_transition() {
    let (_database, service, clock) = setup().await;
    let mut request = add_request(
        "snooze-expiry-create",
        Trigger::AtTime {
            at: datetime!(2026-07-19 05:00 UTC),
        },
    );
    request.overdue_after_seconds = 0;
    let created = service.add(&actor(), request).await.expect("add");
    let due = service
        .check(&actor(), check_request(50))
        .await
        .expect("due");
    assert_eq!(due.items[0].remindi.version, 2);
    let overdue = service
        .check(&actor(), check_request(50))
        .await
        .expect("overdue");
    assert_eq!(overdue.items[0].remindi.version, 3);
    service
        .snooze(
            &actor(),
            remindi::remindi::SnoozeRequest {
                remindi_id: created.remindi.id,
                expected_version: 3,
                snooze_until: datetime!(2026-07-19 07:00 UTC),
                reason: "Wait for another observation".into(),
                idempotency_key: "snooze-expiry".into(),
            },
            Duration::from_secs(7200),
        )
        .await
        .expect("snooze");
    clock.set(datetime!(2026-07-19 08:00 UTC));

    let resurfaced = service
        .check(&actor(), check_request(50))
        .await
        .expect("resurface");
    assert_eq!(resurfaced.items[0].remindi.version, 5);
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
    assert_eq!(history.items.len(), 5);
    assert_eq!(
        history.items.last().expect("last event").prior_version,
        Some(4)
    );
    assert_eq!(
        history.items.last().expect("last event").new_version,
        Some(5)
    );
}

#[tokio::test]
async fn persisted_trigger_and_recurrence_timestamps_are_canonical_utc_milliseconds() {
    let (database, service, _clock) = setup().await;
    let mut request = add_request(
        "canonical-timestamps",
        Trigger::Interval {
            first_at: datetime!(2026-07-19 08:00:00.123456 +02:00),
            every_seconds: 3600,
        },
    );
    request.recurrence = Some(RecurrenceSpec {
        every_seconds: 3600,
        missed_policy: MissedPolicy::Coalesce,
        max_occurrences: None,
        end_at: Some(datetime!(2026-07-20 08:00:00.987654 +02:00)),
    });
    let created = service.add(&actor(), request).await.expect("add");
    let mut connection = database.connection().await.expect("connection");
    let row = sqlx::query(
        "SELECT trigger_spec_json, recurrence_spec_json FROM remindi
         WHERE owner_id = ? AND id = ?",
    )
    .bind("owner-a")
    .bind(created.remindi.id.to_string())
    .fetch_one(connection.as_mut())
    .await
    .expect("row");
    let trigger: Value = serde_json::from_str(row.get("trigger_spec_json")).expect("trigger json");
    let recurrence: Value = serde_json::from_str(
        row.get::<Option<&str>, _>("recurrence_spec_json")
            .expect("recurrence json"),
    )
    .expect("recurrence");

    assert_eq!(trigger["first_at"], "2026-07-19T06:00:00.123Z");
    assert_eq!(recurrence["end_at"], "2026-07-20T06:00:00.987Z");
}

#[tokio::test]
async fn completion_event_identifies_its_evidence_record() {
    let (_database, service, _clock) = setup().await;
    let created = service
        .add(
            &actor(),
            add_request(
                "audit-completion-create",
                Trigger::AtTime {
                    at: datetime!(2026-07-20 06:00 UTC),
                },
            ),
        )
        .await
        .expect("add");
    service
        .complete(
            &actor(),
            CompleteRequest {
                remindi_id: created.remindi.id,
                expected_version: 1,
                evidence: EvidenceInput {
                    evidence_type: EvidenceType::TestResult,
                    summary: "Regression suite passed".into(),
                    reference_uri: Some("https://example.invalid/run/phase2".into()),
                    content_hash: None,
                    observed_at: datetime!(2026-07-19 06:00 UTC),
                    metadata: None,
                    source: EvidenceSource::AuthenticatedActor,
                },
                completion_note: Some("Verified".into()),
                idempotency_key: "audit-completion".into(),
            },
            Duration::from_secs(30),
        )
        .await
        .expect("complete");
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
    let evidence = history.evidence.expect("evidence");
    let completed = history.items.last().expect("completion event");

    assert_eq!(
        completed.details["evidence_id"],
        Value::String(evidence.id.to_string())
    );
}

#[tokio::test]
async fn update_event_records_changed_fields() {
    let (_database, service, _clock) = setup().await;
    let created = service
        .add(
            &actor(),
            add_request(
                "audit-update-create",
                Trigger::AtTime {
                    at: datetime!(2026-07-20 06:00 UTC),
                },
            ),
        )
        .await
        .expect("add");
    let mut update = update_request(created.remindi.id, 1, "audit-update");
    update.message = Some("Collect the final evidence".into());
    service.update(&actor(), update).await.expect("update");
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
    let updated = history.items.last().expect("update event");
    let changed = updated.details["changed_fields"]
        .as_array()
        .expect("changed fields");

    assert!(changed.iter().any(|field| field == "message"));
}

#[tokio::test]
async fn cancellation_reason_is_bounded_at_the_service_boundary() {
    let (_database, service, _clock) = setup().await;
    let created = service
        .add(
            &actor(),
            add_request(
                "bounded-reason-create",
                Trigger::AtTime {
                    at: datetime!(2026-07-20 06:00 UTC),
                },
            ),
        )
        .await
        .expect("add");

    assert!(matches!(
        service
            .cancel(
                &actor(),
                CancelRequest {
                    remindi_id: created.remindi.id,
                    expected_version: 1,
                    reason: "x".repeat(4097),
                    idempotency_key: "bounded-reason".into(),
                },
            )
            .await,
        Err(ServiceError::Validation)
    ));
}
