use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use remindi::{
    clock::{FixedClock, IdGenerator},
    db::DatabaseManager,
    remindi::{
        Actor, AddRequest, CheckRequest, HistoryRequest, LifecycleEvent, ListRequest, Priority,
        RemindiService, Trigger,
    },
    scheduler::{Scheduler, SchedulerConfig},
    triggers::adapters::AdapterRegistry,
};
use serde_json::{Value, json};
use sqlx::Row;
use time::macros::datetime;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const ITEM_COUNT: i64 = 1_000_000;
const ACTIVE_COUNT: i64 = 100_000;
const READY_COUNT: i64 = 20_000;
const PROJECT_COUNT: i64 = 10;
const EVENTS_PER_ITEM: i64 = 20;
const CHUNK_ITEMS: i64 = 10_000;
const NOW_TEXT: &str = "2026-07-19T00:00:00.000Z";
const FUTURE_TEXT: &str = "2027-07-19T00:00:00.000Z";
const OWNER: &str = "performance-owner";

#[derive(Default)]
struct SequenceIds(AtomicU64);

impl SequenceIds {
    fn after_reference_dataset() -> Self {
        Self(AtomicU64::new(2_000_000))
    }
}

impl IdGenerator for SequenceIds {
    fn next_id(&self) -> Uuid {
        Uuid::from_u128(u128::from(self.0.fetch_add(1, Ordering::Relaxed)))
    }
}

#[derive(Clone, Copy, Debug)]
struct Sizes {
    database: u64,
    wal: u64,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "SPEC 23.8 creates 1M items and 20M events; run explicitly in release mode"]
async fn spec_23_8_reference_performance() {
    assert_eq!(
        std::env::var("REMINDI_RUN_REFERENCE_PERFORMANCE").as_deref(),
        Ok("1"),
        "set REMINDI_RUN_REFERENCE_PERFORMANCE=1 to acknowledge the large reference workload"
    );

    let root = std::env::var_os("REMINDI_PERF_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&root).expect("performance root exists");
    let directory = tempfile::Builder::new()
        .prefix("remindi-reference-performance-")
        .tempdir_in(root)
        .expect("isolated performance directory");
    let path = directory.path().join("remindi.db");
    let database = Arc::new(
        DatabaseManager::open(&path)
            .await
            .expect("reference database opens"),
    );
    let empty_sizes = sizes(&path);

    let item_started = Instant::now();
    insert_items(&database).await;
    let item_seed_seconds = item_started.elapsed().as_secs_f64();
    let item_sizes = sizes(&path);

    let event_started = Instant::now();
    insert_events(&database).await;
    let event_seed_seconds = event_started.elapsed().as_secs_f64();
    let reference_sizes = sizes(&path);
    let counts = verify_reference_counts(&database).await;

    let clock = Arc::new(FixedClock::new(datetime!(2026-07-19 00:00 UTC)));
    let service = Arc::new(RemindiService::new(
        Arc::clone(&database),
        OWNER,
        b"reference-performance-cursor-secret",
        clock.clone(),
        Arc::new(SequenceIds::after_reference_dataset()),
    ));
    let actor = Actor::agent("performance-agent", Some("performance-run".into()));

    let project_list = measure_project_list(&service, &actor, None).await;
    let task_list = measure_project_list(&service, &actor, Some("task-01")).await;
    let due_candidates = measure_due_candidates(&database).await;
    let project_check = measure_project_check(&service, &actor).await;
    let history = measure_history(&service, &actor).await;
    let (scheduler_seconds, scheduler_selected, scheduler_applied) =
        measure_scheduler(&database, &service, clock).await;
    let (write_under_read, reader_iterations) =
        measure_write_under_read(&database, &service, &actor).await;
    let final_sizes = sizes(&path);

    let sqlite_version: String = {
        let mut connection = database.connection().await.expect("version connection");
        sqlx::query_scalar("SELECT sqlite_version()")
            .fetch_one(connection.as_mut())
            .await
            .expect("SQLite version")
    };
    let report = json!({
        "requirement": "SPEC-23.8",
        "dataset": counts,
        "seed": {
            "items_seconds": item_seed_seconds,
            "events_seconds": event_seed_seconds,
        },
        "latency_ms": {
            "project_list": summary(&project_list),
            "project_task_list": summary(&task_list),
            "project_check": summary(&project_check),
            "due_candidate_query": summary(&due_candidates),
            "history_pagination": summary(&history),
            "write_under_four_reads": summary(&write_under_read),
        },
        "target": {
            "project_check_p95_under_ms": 250.0,
            "measured_p95_ms": p95_ms(&project_check),
            "passed": p95_ms(&project_check) < 250.0,
        },
        "scheduler": {
            "seconds": scheduler_seconds,
            "selected": scheduler_selected,
            "applied": scheduler_applied,
            "evaluations_per_second": scheduler_applied as f64 / scheduler_seconds,
        },
        "concurrent_read_iterations": reader_iterations,
        "storage_bytes": {
            "empty": size_json(empty_sizes),
            "after_items": size_json(item_sizes),
            "reference_dataset": size_json(reference_sizes),
            "after_measurements": size_json(final_sizes),
        },
        "versions": {
            "rust": env!("CARGO_PKG_RUST_VERSION"),
            "sqlite": sqlite_version,
        }
    });
    println!(
        "REMINDI_REFERENCE_PERFORMANCE={}",
        serde_json::to_string_pretty(&report).expect("report serializes")
    );

    assert_eq!(counts["items"], ITEM_COUNT);
    assert_eq!(counts["active"], ACTIVE_COUNT);
    assert_eq!(counts["projects"], PROJECT_COUNT);
    assert_eq!(counts["ready"], READY_COUNT);
    assert_eq!(counts["events"], ITEM_COUNT * EVENTS_PER_ITEM);
    assert_eq!(counts["events_per_item"], 20.0);
    assert_eq!(scheduler_selected, READY_COUNT as usize);
    assert_eq!(scheduler_applied, READY_COUNT as usize);

    drop(service);
    Arc::try_unwrap(database)
        .expect("sole database owner")
        .close()
        .await
        .expect("database closes");
}

async fn insert_items(database: &DatabaseManager) {
    let mut start = 1;
    while start <= ITEM_COUNT {
        let end = (start + CHUNK_ITEMS - 1).min(ITEM_COUNT);
        let mut transaction = database.begin_immediate().await.expect("item transaction");
        sqlx::query(
            "WITH RECURSIVE seq(n) AS (
                 SELECT ?1
                 UNION ALL
                 SELECT n + 1 FROM seq WHERE n < ?2
             )
             INSERT INTO remindi (
                 id, owner_id, project_id, task_id, message, instructions, state, priority,
                 trigger_type, trigger_spec_json, recurrence_spec_json, next_fire_at,
                 next_evaluation_at, original_next_fire_at, due_since, snooze_until,
                 snoozed_from_state, overdue_after_seconds, occurrence_no, source_session_id,
                 source_task_lineage_id, last_checked_at, last_condition_status,
                 last_condition_detail, snooze_count, version, created_at, updated_at,
                 completed_at, cancelled_at
             )
             SELECT
                 printf('00000000-0000-7000-8000-%012x', n),
                 'performance-owner',
                 printf('project-%02d', ((n - 1) % 10) + 1),
                 printf('task-%02d', ((n - 1) % 100) + 1),
                 printf('Reference Remindi item %d', n),
                 NULL,
                 CASE WHEN n <= 100000 THEN 'scheduled'
                      ELSE 'completed' END,
                 'normal',
                 'at_time',
                 CASE WHEN n <= 20000
                      THEN '{\"type\":\"at_time\",\"at\":\"2026-07-19T00:00:00Z\"}'
                      ELSE '{\"type\":\"at_time\",\"at\":\"2027-07-19T00:00:00Z\"}' END,
                 NULL,
                 CASE WHEN n <= 20000 THEN ?3
                      WHEN n <= 100000 THEN ?4
                      ELSE NULL END,
                 NULL,
                 NULL,
                 NULL,
                 NULL,
                 NULL,
                 86400,
                 1,
                 NULL,
                 NULL,
                 NULL,
                 NULL,
                 NULL,
                 0,
                 1,
                 ?3,
                 ?3,
                 CASE WHEN n > 100000 THEN ?3 ELSE NULL END,
                 NULL
             FROM seq",
        )
        .bind(start)
        .bind(end)
        .bind(NOW_TEXT)
        .bind(FUTURE_TEXT)
        .execute(transaction.as_mut())
        .await
        .expect("item chunk inserts");
        transaction.commit().await.expect("item chunk commits");
        start = end + 1;
    }
}

async fn insert_events(database: &DatabaseManager) {
    let mut start = 1;
    while start <= ITEM_COUNT {
        let end = (start + CHUNK_ITEMS - 1).min(ITEM_COUNT);
        let mut transaction = database.begin_immediate().await.expect("event transaction");
        sqlx::query(
            "WITH RECURSIVE
             items(n) AS (
                 SELECT ?1
                 UNION ALL
                 SELECT n + 1 FROM items WHERE n < ?2
             ),
             event_numbers(e) AS (
                 SELECT 1
                 UNION ALL
                 SELECT e + 1 FROM event_numbers WHERE e < 20
             )
             INSERT INTO remindi_events (
                 event_id, remindi_id, event_type, actor_type, actor_id, request_id,
                 occurred_at, prior_version, new_version, details_json
             )
             SELECT
                 printf('10000000-0000-7000-8000-%012x', ((n - 1) * 20) + e),
                 printf('00000000-0000-7000-8000-%012x', n),
                 CASE WHEN e = 1 THEN 'created' ELSE 'checked' END,
                 'system',
                 'reference-generator',
                 NULL,
                 ?3,
                 NULL,
                 1,
                 '{}'
             FROM items CROSS JOIN event_numbers",
        )
        .bind(start)
        .bind(end)
        .bind(NOW_TEXT)
        .execute(transaction.as_mut())
        .await
        .expect("event chunk inserts");
        transaction.commit().await.expect("event chunk commits");
        start = end + 1;
    }
}

async fn verify_reference_counts(database: &DatabaseManager) -> Value {
    let mut connection = database.connection().await.expect("count connection");
    let row = sqlx::query(
        "SELECT
             COUNT(*) AS items,
             SUM(state IN ('scheduled', 'due', 'overdue', 'snoozed')) AS active,
             COUNT(DISTINCT project_id) AS projects,
             SUM(
                 state = 'scheduled'
                 AND julianday(next_fire_at) <= julianday(?)
             ) AS ready
         FROM remindi",
    )
    .bind(NOW_TEXT)
    .fetch_one(connection.as_mut())
    .await
    .expect("item counts");
    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM remindi_events")
        .fetch_one(connection.as_mut())
        .await
        .expect("event count");
    let items: i64 = row.get("items");
    json!({
        "items": items,
        "active": row.get::<i64, _>("active"),
        "projects": row.get::<i64, _>("projects"),
        "ready": row.get::<i64, _>("ready"),
        "events": events,
        "events_per_item": events as f64 / items as f64,
    })
}

async fn measure_project_list(
    service: &RemindiService,
    actor: &Actor,
    task_id: Option<&str>,
) -> Vec<Duration> {
    let mut samples = Vec::with_capacity(40);
    for _ in 0..40 {
        let started = Instant::now();
        let page = service
            .list(
                actor,
                ListRequest {
                    project_id: Some("project-01".into()),
                    task_id: task_id.map(str::to_owned),
                    states: vec![],
                    trigger_types: vec![],
                    linked_goal_id: None,
                    linked_memory_hash: None,
                    limit: 100,
                    cursor: None,
                },
            )
            .await
            .expect("project list succeeds");
        samples.push(started.elapsed());
        assert_eq!(page.items.len(), 100);
    }
    samples
}

async fn measure_project_check(service: &RemindiService, actor: &Actor) -> Vec<Duration> {
    let mut samples = Vec::with_capacity(40);
    for sample in 0..40 {
        let started = Instant::now();
        let checked = service
            .check(
                actor,
                CheckRequest {
                    project_id: format!("project-{:02}", (sample % 10) + 1),
                    task_id: None,
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
            .expect("project check succeeds");
        samples.push(started.elapsed());
        assert_eq!(checked.items.len(), 50);
    }
    samples
}

async fn measure_due_candidates(database: &DatabaseManager) -> Vec<Duration> {
    const QUERY: &str = "SELECT r.* FROM remindi r
        WHERE r.owner_id = ?
          AND (
            (r.state = 'snoozed' AND julianday(r.snooze_until) <= julianday(?))
            OR (
              r.state = 'due'
              AND r.due_since IS NOT NULL
              AND julianday(r.due_since) + (r.overdue_after_seconds / 86400.0)
                  <= julianday(?)
            )
            OR (
              r.state = 'scheduled'
              AND (
                (
                  r.trigger_type IN ('at_time', 'after_elapsed', 'interval')
                  AND julianday(r.next_fire_at) <= julianday(?)
                )
                OR (
                  r.trigger_type = 'condition'
                  AND (
                    julianday(r.next_evaluation_at) <= julianday(?)
                    OR julianday(json_extract(r.trigger_spec_json, '$.manual_check_at'))
                       <= julianday(?)
                  )
                )
              )
            )
          )
        ORDER BY
          CASE
            WHEN r.state = 'snoozed' THEN r.snooze_until
            WHEN r.state = 'due' THEN r.due_since
            WHEN r.trigger_type = 'condition' THEN
              CASE
                WHEN r.next_evaluation_at IS NULL THEN
                  json_extract(r.trigger_spec_json, '$.manual_check_at')
                WHEN json_extract(r.trigger_spec_json, '$.manual_check_at') IS NULL THEN
                  r.next_evaluation_at
                WHEN julianday(r.next_evaluation_at)
                     <= julianday(json_extract(r.trigger_spec_json, '$.manual_check_at'))
                  THEN r.next_evaluation_at
                ELSE json_extract(r.trigger_spec_json, '$.manual_check_at')
              END
            ELSE r.next_fire_at
          END ASC,
          r.id ASC
        LIMIT ?";
    let mut samples = Vec::with_capacity(40);
    for _ in 0..40 {
        let mut connection = database.connection().await.expect("candidate connection");
        let started = Instant::now();
        let rows = sqlx::query(QUERY)
            .bind(OWNER)
            .bind(NOW_TEXT)
            .bind(NOW_TEXT)
            .bind(NOW_TEXT)
            .bind(NOW_TEXT)
            .bind(NOW_TEXT)
            .bind(500_i64)
            .fetch_all(connection.as_mut())
            .await
            .expect("candidate query succeeds");
        samples.push(started.elapsed());
        assert_eq!(rows.len(), 500);
    }
    samples
}

async fn measure_history(service: &RemindiService, actor: &Actor) -> Vec<Duration> {
    let mut samples = Vec::with_capacity(40);
    for _ in 0..40 {
        let started = Instant::now();
        let page = service
            .history(
                actor,
                HistoryRequest {
                    remindi_id: Uuid::parse_str("00000000-0000-7000-8000-000000000001")
                        .expect("reference UUID"),
                    after_sequence: Some(10),
                    event_types: vec![],
                    limit: 10,
                    cursor: None,
                },
            )
            .await
            .expect("history succeeds");
        samples.push(started.elapsed());
        assert_eq!(page.items.len(), 10);
    }
    samples
}

async fn measure_write_under_read(
    database: &Arc<DatabaseManager>,
    service: &Arc<RemindiService>,
    actor: &Actor,
) -> (Vec<Duration>, u64) {
    let stop = CancellationToken::new();
    let read_count = Arc::new(AtomicU64::new(0));
    let mut readers = Vec::new();
    for _ in 0..4 {
        let database = Arc::clone(database);
        let stop = stop.child_token();
        let read_count = Arc::clone(&read_count);
        readers.push(tokio::spawn(async move {
            while !stop.is_cancelled() {
                let mut connection = database.connection().await.expect("reader connection");
                let _: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM remindi
                     WHERE owner_id = ? AND project_id = ?
                       AND state IN ('scheduled', 'due', 'overdue', 'snoozed')",
                )
                .bind(OWNER)
                .bind("project-01")
                .fetch_one(connection.as_mut())
                .await
                .expect("concurrent read");
                read_count.fetch_add(1, Ordering::Relaxed);
                tokio::task::yield_now().await;
            }
        }));
    }

    let mut samples = Vec::with_capacity(40);
    for index in 0..40 {
        let started = Instant::now();
        service
            .add(
                actor,
                AddRequest {
                    project_id: "performance-writes".into(),
                    task_id: Some("concurrent-read".into()),
                    message: format!("Write-under-read sample {index}"),
                    instructions: None,
                    priority: Priority::Normal,
                    trigger: Trigger::AtTime {
                        at: datetime!(2027-07-19 00:00 UTC),
                    },
                    recurrence: None,
                    overdue_after_seconds: 86_400,
                    links: vec![],
                    session_id: None,
                    task_lineage_id: None,
                    idempotency_key: format!("performance-write-{index}"),
                },
            )
            .await
            .expect("write-under-read succeeds");
        samples.push(started.elapsed());
    }
    stop.cancel();
    for reader in readers {
        reader.await.expect("reader task joins");
    }
    (samples, read_count.load(Ordering::Relaxed))
}

async fn measure_scheduler(
    database: &Arc<DatabaseManager>,
    service: &Arc<RemindiService>,
    clock: Arc<FixedClock>,
) -> (f64, usize, usize) {
    let mut transaction = database
        .begin_immediate()
        .await
        .expect("scheduler preparation transaction");
    sqlx::query("DELETE FROM remindi_events WHERE sequence > ?")
        .bind(ITEM_COUNT * EVENTS_PER_ITEM)
        .execute(transaction.as_mut())
        .await
        .expect("project-check measurement events reset");
    sqlx::query(
        "UPDATE remindi
         SET state = 'scheduled',
             next_fire_at = ?,
             due_since = NULL,
             overdue_after_seconds = 86400,
             version = 1,
             updated_at = ?
         WHERE owner_id = ?
           AND id <= '00000000-0000-7000-8000-000000004e20'",
    )
    .bind(NOW_TEXT)
    .bind(NOW_TEXT)
    .bind(OWNER)
    .execute(transaction.as_mut())
    .await
    .expect("scheduler candidates prepared");
    transaction
        .commit()
        .await
        .expect("scheduler preparation commits");

    let scheduler = Scheduler::new(
        Arc::clone(database),
        Arc::clone(service),
        Arc::new(AdapterRegistry::disabled(clock.clone())),
        clock,
        "reference-performance",
        SchedulerConfig {
            poll_interval: Duration::from_secs(1),
            lease_duration: Duration::from_secs(3_600),
            adapter_timeout: Duration::from_secs(5),
            adapter_concurrency: 8,
            candidate_batch_size: 500,
        },
    )
    .expect("scheduler builds");
    let mut guard = scheduler.acquire().await.expect("scheduler lease");
    let started = Instant::now();
    let mut selected = 0;
    let mut applied = 0;
    loop {
        let report = scheduler
            .poll_once(&mut guard, CancellationToken::new())
            .await
            .expect("scheduler poll succeeds");
        if report.selected == 0 {
            break;
        }
        selected += report.selected;
        applied += report.applied;
        assert_eq!(report.failures, 0);
        assert_eq!(report.conflicts, 0);
    }
    let seconds = started.elapsed().as_secs_f64();
    scheduler.release(&guard).await.expect("scheduler releases");
    (seconds, selected, applied)
}

fn p95_ms(samples: &[Duration]) -> f64 {
    let mut milliseconds = samples
        .iter()
        .map(|sample| sample.as_secs_f64() * 1_000.0)
        .collect::<Vec<_>>();
    milliseconds.sort_by(f64::total_cmp);
    let rank = (milliseconds.len() * 95).div_ceil(100);
    milliseconds[rank.saturating_sub(1)]
}

fn summary(samples: &[Duration]) -> Value {
    let mut milliseconds = samples
        .iter()
        .map(|sample| sample.as_secs_f64() * 1_000.0)
        .collect::<Vec<_>>();
    milliseconds.sort_by(f64::total_cmp);
    json!({
        "samples": milliseconds.len(),
        "min": milliseconds.first().copied().unwrap_or(0.0),
        "median": milliseconds[milliseconds.len() / 2],
        "p95_nearest_rank": p95_ms(samples),
        "max": milliseconds.last().copied().unwrap_or(0.0),
    })
}

fn sizes(path: &Path) -> Sizes {
    Sizes {
        database: std::fs::metadata(path).map_or(0, |metadata| metadata.len()),
        wal: std::fs::metadata(format!("{}-wal", path.display()))
            .map_or(0, |metadata| metadata.len()),
    }
}

fn size_json(sizes: Sizes) -> Value {
    json!({
        "database": sizes.database,
        "wal": sizes.wal,
        "combined": sizes.database + sizes.wal,
    })
}
