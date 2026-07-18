use std::{fs, path::PathBuf, str::FromStr};

use remindi::db::DatabaseManager;
use sha2::{Digest, Sha256};
use sqlx::{Connection, Row, sqlite::SqliteConnection};
use uuid::Uuid;

fn temporary_directory(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("remindi-{label}-{}", Uuid::now_v7()));
    fs::create_dir_all(&path).expect("temporary directory is created");
    path
}

fn database_path(label: &str) -> PathBuf {
    temporary_directory(label).join("remindi.db")
}

async fn table_names(manager: &DatabaseManager) -> Vec<String> {
    let mut connection = manager.connection().await.expect("database connection");
    sqlx::query_scalar::<_, String>(
        "SELECT name FROM sqlite_master \
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )
    .fetch_all(connection.as_mut())
    .await
    .expect("table names")
}

#[tokio::test]
async fn fresh_database_applies_exact_version_two_schema_and_seed_rows() {
    let path = database_path("fresh");
    let manager = DatabaseManager::open(&path)
        .await
        .expect("fresh database opens");

    assert_eq!(
        table_names(&manager).await,
        vec![
            "adapter_configs",
            "admin_events",
            "backup_records",
            "completion_evidence",
            "idempotency_records",
            "remindi",
            "remindi_events",
            "remindi_links",
            "runtime_settings",
            "scheduler_leases",
            "schema_migrations",
            "service_runtime",
        ]
    );

    let mut connection = manager.connection().await.expect("database connection");
    let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM schema_migrations")
        .fetch_one(connection.as_mut())
        .await
        .expect("schema version");
    let runtime_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runtime_settings")
        .fetch_one(connection.as_mut())
        .await
        .expect("runtime settings count");
    let adapter_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM adapter_configs")
        .fetch_one(connection.as_mut())
        .await
        .expect("adapter config count");
    let workload_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM service_runtime")
        .fetch_one(connection.as_mut())
        .await
        .expect("workload count");

    assert_eq!(
        (version, runtime_count, adapter_count, workload_count),
        (2, 11, 4, 2)
    );
}

#[tokio::test]
async fn every_connection_enforces_required_sqlite_pragmas() {
    let path = database_path("pragmas");
    let manager = DatabaseManager::open(&path)
        .await
        .expect("fresh database opens");
    let mut connection = manager.connection().await.expect("database connection");

    let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(connection.as_mut())
        .await
        .expect("journal mode");
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(connection.as_mut())
        .await
        .expect("foreign keys");
    let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
        .fetch_one(connection.as_mut())
        .await
        .expect("synchronous");
    let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
        .fetch_one(connection.as_mut())
        .await
        .expect("busy timeout");

    assert_eq!(
        (
            journal_mode.as_str(),
            foreign_keys,
            synchronous,
            busy_timeout
        ),
        ("wal", 1, 2, 5000)
    );
}

#[tokio::test]
async fn immediate_transaction_rolls_back_item_and_event_together() {
    let path = database_path("transaction");
    let manager = DatabaseManager::open(&path)
        .await
        .expect("fresh database opens");
    let mut transaction = manager
        .begin_immediate()
        .await
        .expect("immediate transaction");

    sqlx::query(
        "INSERT INTO remindi (
            id, owner_id, project_id, message, state, priority, trigger_type,
            trigger_spec_json, next_fire_at, created_at, updated_at
         ) VALUES (?, ?, ?, ?, 'scheduled', 'normal', 'at_time', '{}', ?, ?, ?)",
    )
    .bind("00000000-0000-7000-8000-000000000001")
    .bind("owner")
    .bind("project")
    .bind("message")
    .bind("2026-07-19T00:00:00.000Z")
    .bind("2026-07-18T00:00:00.000Z")
    .bind("2026-07-18T00:00:00.000Z")
    .execute(transaction.as_mut())
    .await
    .expect("item insert");
    sqlx::query(
        "INSERT INTO remindi_events (
            event_id, remindi_id, event_type, actor_type, actor_id, occurred_at,
            details_json
         ) VALUES (?, ?, 'created', 'system', 'system', ?, '{}')",
    )
    .bind("00000000-0000-7000-8000-000000000002")
    .bind("00000000-0000-7000-8000-000000000001")
    .bind("2026-07-18T00:00:00.000Z")
    .execute(transaction.as_mut())
    .await
    .expect("event insert");
    sqlx::query(
        "INSERT INTO completion_evidence (
            id, remindi_id, evidence_type, summary, reference_uri, observed_at,
            recorded_at, recorded_by
         ) VALUES (?, ?, 'test_result', 'passing test', 'file:///test-report',
                   ?, ?, 'system')",
    )
    .bind("00000000-0000-7000-8000-000000000003")
    .bind("00000000-0000-7000-8000-000000000001")
    .bind("2026-07-18T00:00:00.000Z")
    .bind("2026-07-18T00:00:00.000Z")
    .execute(transaction.as_mut())
    .await
    .expect("evidence insert");
    sqlx::query(
        "INSERT INTO idempotency_records (
            actor_id, tool_name, idempotency_key, request_hash, response_json,
            remindi_id, created_at, expires_at
         ) VALUES ('system', 'test', 'test-key', 'hash', '{}', ?, ?, ?)",
    )
    .bind("00000000-0000-7000-8000-000000000001")
    .bind("2026-07-18T00:00:00.000Z")
    .bind("2026-08-18T00:00:00.000Z")
    .execute(transaction.as_mut())
    .await
    .expect("idempotency insert");
    transaction.rollback().await.expect("rollback");

    let mut connection = manager.connection().await.expect("database connection");
    let counts: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT
            (SELECT COUNT(*) FROM remindi),
            (SELECT COUNT(*) FROM remindi_events),
            (SELECT COUNT(*) FROM completion_evidence),
            (SELECT COUNT(*) FROM idempotency_records)",
    )
    .fetch_one(connection.as_mut())
    .await
    .expect("transactional row counts");
    assert_eq!(counts, (0, 0, 0, 0));
}

#[tokio::test]
async fn startup_refuses_a_newer_unknown_schema() {
    let path = database_path("newer");
    let options = sqlx::sqlite::SqliteConnectOptions::from_str(&format!(
        "sqlite://{}?mode=rwc",
        path.display()
    ))
    .expect("SQLite URL");
    let mut connection = SqliteConnection::connect_with(&options)
        .await
        .expect("raw database opens");
    sqlx::raw_sql(
        "CREATE TABLE schema_migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at TEXT NOT NULL
         ) STRICT;
         INSERT INTO schema_migrations VALUES (999, 'future', '2026-07-18T00:00:00.000Z');",
    )
    .execute(&mut connection)
    .await
    .expect("future schema seeded");
    connection.close().await.expect("raw database closes");

    let error = DatabaseManager::open(&path)
        .await
        .expect_err("newer schema must be refused");
    assert!(error.to_string().contains("newer schema version 999"));
}

#[tokio::test]
async fn version_one_database_upgrades_to_version_two() {
    let path = database_path("upgrade");
    let options = sqlx::sqlite::SqliteConnectOptions::from_str(&format!(
        "sqlite://{}?mode=rwc",
        path.display()
    ))
    .expect("SQLite URL");
    let mut connection = SqliteConnection::connect_with(&options)
        .await
        .expect("raw database opens");
    let migration = include_str!("../../migrations/0001_initial.sql");
    sqlx::raw_sql(
        "CREATE TABLE schema_migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at TEXT NOT NULL
         ) STRICT;",
    )
    .execute(&mut connection)
    .await
    .expect("migration table created");
    sqlx::raw_sql(migration)
        .execute(&mut connection)
        .await
        .expect("version one schema applied");
    sqlx::query("INSERT INTO schema_migrations VALUES (1, ?, '2026-07-18T00:00:00.000Z')")
        .bind(format!(
            "initial:{:x}",
            Sha256::digest(migration.as_bytes())
        ))
        .execute(&mut connection)
        .await
        .expect("version one recorded");
    connection.close().await.expect("raw database closes");

    let manager = DatabaseManager::open(&path)
        .await
        .expect("supported database upgrades");
    assert_eq!(table_names(&manager).await.len(), 12);
}

#[tokio::test]
async fn startup_refuses_migration_drift() {
    let path = database_path("drift");
    let options = sqlx::sqlite::SqliteConnectOptions::from_str(&format!(
        "sqlite://{}?mode=rwc",
        path.display()
    ))
    .expect("SQLite URL");
    let mut connection = SqliteConnection::connect_with(&options)
        .await
        .expect("raw database opens");
    sqlx::raw_sql(
        "CREATE TABLE schema_migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at TEXT NOT NULL
         ) STRICT;
         INSERT INTO schema_migrations VALUES (1, 'initial:wrong', '2026-07-18T00:00:00.000Z');",
    )
    .execute(&mut connection)
    .await
    .expect("drifted migration seeded");
    connection.close().await.expect("raw database closes");

    let error = DatabaseManager::open(&path)
        .await
        .expect_err("migration drift must be refused");
    assert!(error.to_string().contains("migration 1 has drifted"));
}

#[tokio::test]
async fn schema_constraints_reject_invalid_item_state() {
    let path = database_path("constraints");
    let manager = DatabaseManager::open(&path)
        .await
        .expect("fresh database opens");
    let mut connection = manager.connection().await.expect("database connection");
    let error = sqlx::query(
        "INSERT INTO remindi (
            id, owner_id, project_id, message, state, trigger_type,
            trigger_spec_json, created_at, updated_at
         ) VALUES ('id', 'owner', 'project', 'message', 'invalid', 'at_time',
                   '{}', '2026-07-18T00:00:00.000Z', '2026-07-18T00:00:00.000Z')",
    )
    .execute(connection.as_mut())
    .await
    .expect_err("invalid state must violate the schema");

    assert!(error.to_string().contains("CHECK constraint failed"));
}

#[tokio::test]
async fn startup_repairs_missing_required_control_rows() {
    let path = database_path("repair-seeds");
    DatabaseManager::open(&path)
        .await
        .expect("fresh database opens")
        .close()
        .await
        .expect("database closes");
    let mut connection = SqliteConnection::connect(&format!("sqlite://{}", path.display()))
        .await
        .expect("database opens directly");
    sqlx::query("DELETE FROM runtime_settings WHERE setting_key = ?")
        .bind("scheduler.poll_interval_seconds")
        .execute(&mut connection)
        .await
        .expect("required row removed");
    connection.close().await.expect("raw database closes");

    let manager = DatabaseManager::open(&path)
        .await
        .expect("missing required row is restored");
    let mut connection = manager.connection().await.expect("database connection");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runtime_settings")
        .fetch_one(connection.as_mut())
        .await
        .expect("runtime settings count");
    assert_eq!(count, 11);
}

#[tokio::test]
async fn startup_rejects_unknown_or_malformed_control_rows() {
    let unknown_path = database_path("unknown-seed");
    DatabaseManager::open(&unknown_path)
        .await
        .expect("fresh database opens")
        .close()
        .await
        .expect("database closes");
    let mut connection = SqliteConnection::connect(&format!("sqlite://{}", unknown_path.display()))
        .await
        .expect("database opens directly");
    sqlx::query(
        "INSERT INTO runtime_settings (
            setting_key, value_json, updated_at, updated_by
         ) VALUES ('unknown.setting', '1', '2026-07-18T00:00:00.000Z', 'test')",
    )
    .execute(&mut connection)
    .await
    .expect("unknown row inserted");
    connection.close().await.expect("raw database closes");
    assert!(
        DatabaseManager::open(&unknown_path)
            .await
            .expect_err("unknown row must be refused")
            .to_string()
            .contains("bootstrap control rows are invalid")
    );

    let malformed_path = database_path("malformed-seed");
    DatabaseManager::open(&malformed_path)
        .await
        .expect("fresh database opens")
        .close()
        .await
        .expect("database closes");
    let mut connection =
        SqliteConnection::connect(&format!("sqlite://{}", malformed_path.display()))
            .await
            .expect("database opens directly");
    sqlx::query(
        "UPDATE runtime_settings SET value_json = '\"not-an-integer\"'
         WHERE setting_key = 'scheduler.poll_interval_seconds'",
    )
    .execute(&mut connection)
    .await
    .expect("malformed row inserted");
    connection.close().await.expect("raw database closes");
    assert!(
        DatabaseManager::open(&malformed_path)
            .await
            .expect_err("malformed row must be refused")
            .to_string()
            .contains("bootstrap control rows are invalid")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn startup_refuses_world_writable_data_directory() {
    use std::os::unix::fs::PermissionsExt;

    let directory = temporary_directory("permissions");
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o777))
        .expect("permissions changed");
    let error = DatabaseManager::open(&directory.join("remindi.db"))
        .await
        .expect_err("world-writable directory must be refused");

    assert!(error.to_string().contains("world-writable"));
}

#[tokio::test]
async fn close_releases_the_pool_and_preserves_integrity() {
    let path = database_path("close");
    let manager = DatabaseManager::open(&path)
        .await
        .expect("fresh database opens");
    manager.close().await.expect("database closes");

    let mut connection = SqliteConnection::connect(&format!("sqlite://{}", path.display()))
        .await
        .expect("database reopens directly");
    let integrity: String = sqlx::query("PRAGMA integrity_check")
        .fetch_one(&mut connection)
        .await
        .expect("integrity result")
        .get(0);
    assert_eq!(integrity, "ok");
}
