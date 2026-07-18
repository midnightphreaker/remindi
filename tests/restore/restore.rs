use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use futures::stream;
use remindi::{
    admin::{
        AdminActor,
        backup::{
            BackupError, BackupManager, BackupSource, RESTORE_CONFIRMATION, RestoreFault,
            RestoreManager,
        },
        workloads::{WorkloadController, WorkloadRuntime},
    },
    clock::{FixedClock, UuidV7Generator},
    db::{DatabaseError, DatabaseManager},
};
use sha2::{Digest, Sha256};
use sqlx::{Connection, sqlite::SqliteConnectOptions};
use std::str::FromStr;
use time::macros::datetime;
use uuid::Uuid;

struct ProbeRuntime(AtomicBool);

#[async_trait]
impl WorkloadRuntime for ProbeRuntime {
    fn is_running(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    async fn start(&self) -> Result<(), String> {
        self.0.store(true, Ordering::Release);
        Ok(())
    }

    async fn stop(&self) -> Result<(), String> {
        self.0.store(false, Ordering::Release);
        Ok(())
    }
}

struct Fixture {
    root: PathBuf,
    database: Arc<DatabaseManager>,
    backups: Arc<BackupManager>,
    restore: RestoreManager,
    mcp: Arc<ProbeRuntime>,
    scheduler: Arc<ProbeRuntime>,
}

fn actor() -> AdminActor {
    AdminActor::new("web:owner", Some("req-restore".to_owned())).expect("actor")
}

async fn fixture(label: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("remindi-restore-{label}-{}", Uuid::now_v7()));
    fs::create_dir_all(&root).expect("root");
    let database = Arc::new(
        DatabaseManager::open(root.join("remindi.db"))
            .await
            .expect("database"),
    );
    let backups = Arc::new(
        BackupManager::open(
            Arc::clone(&database),
            root.join("backups"),
            "owner",
            Arc::new(FixedClock::new(datetime!(2026-07-19 01:00 UTC))),
            Arc::new(UuidV7Generator),
        )
        .await
        .expect("backups"),
    );
    let mcp = Arc::new(ProbeRuntime(AtomicBool::new(true)));
    let scheduler = Arc::new(ProbeRuntime(AtomicBool::new(true)));
    let workloads = Arc::new(
        WorkloadController::from_runtimes(
            Arc::clone(&database),
            Arc::new(FixedClock::new(datetime!(2026-07-19 01:00 UTC))),
            Arc::clone(&mcp) as Arc<dyn WorkloadRuntime>,
            Arc::clone(&scheduler) as Arc<dyn WorkloadRuntime>,
        )
        .await
        .expect("workloads"),
    );
    let restore = RestoreManager::new(Arc::clone(&database), Arc::clone(&backups), workloads);
    Fixture {
        root,
        database,
        backups,
        restore,
        mcp,
        scheduler,
    }
}

async fn setting(database: &DatabaseManager) -> i64 {
    let mut connection = database.connection().await.expect("connection");
    sqlx::query_scalar(
        "SELECT CAST(value_json AS INTEGER) FROM runtime_settings \
         WHERE setting_key = 'remindi.default_overdue_seconds'",
    )
    .fetch_one(connection.as_mut())
    .await
    .expect("setting")
}

async fn set_setting(database: &DatabaseManager, value: i64) {
    let mut transaction = database.begin_immediate().await.expect("transaction");
    sqlx::query(
        "UPDATE runtime_settings SET value_json = ? \
         WHERE setting_key = 'remindi.default_overdue_seconds'",
    )
    .bind(value.to_string())
    .execute(transaction.as_mut())
    .await
    .expect("update");
    transaction.commit().await.expect("commit");
}

async fn version_one_database(path: &std::path::Path) {
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
        .expect("options")
        .create_if_missing(true);
    let mut connection = sqlx::SqliteConnection::connect_with(&options)
        .await
        .expect("v1 opens");
    sqlx::raw_sql(include_str!("../../migrations/0001_initial.sql"))
        .execute(&mut connection)
        .await
        .expect("v1 schema");
    sqlx::raw_sql(
        "CREATE TABLE schema_migrations (\
           version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL\
         ) STRICT;",
    )
    .execute(&mut connection)
    .await
    .expect("migration table");
    let digest = Sha256::digest(include_str!("../../migrations/0001_initial.sql").as_bytes());
    sqlx::query(
        "INSERT INTO schema_migrations(version, name, applied_at) \
         VALUES (1, ?, '2026-07-19T00:00:00.000Z')",
    )
    .bind(format!("initial:{digest:x}"))
    .execute(&mut connection)
    .await
    .expect("migration row");
    connection.close().await.expect("v1 closes");
}

#[tokio::test]
async fn guarded_restore_replaces_valid_database_clears_leases_and_restarts_workloads() {
    let fixture = fixture("success").await;
    let candidate = fixture
        .backups
        .create(BackupSource::Manual, &actor())
        .await
        .expect("candidate");
    set_setting(&fixture.database, 42).await;
    {
        let mut transaction = fixture
            .database
            .begin_immediate()
            .await
            .expect("transaction");
        sqlx::query(
            "INSERT INTO scheduler_leases(\
               lease_name, holder_id, acquired_at, expires_at, version\
             ) VALUES (\
               'scheduler', 'old-process', '2026-07-19T00:00:00.000Z', \
               '2026-07-20T00:00:00.000Z', 1\
             )",
        )
        .execute(transaction.as_mut())
        .await
        .expect("lease");
        transaction.commit().await.expect("commit");
    }

    let outcome = fixture
        .restore
        .restore(
            &candidate.id,
            RESTORE_CONFIRMATION,
            &actor(),
            RestoreFault::None,
        )
        .await
        .expect("restore succeeds");

    assert!(outcome.restored);
    assert_eq!(setting(&fixture.database).await, 0);
    assert!(fixture.mcp.is_running());
    assert!(fixture.scheduler.is_running());
    let mut connection = fixture.database.connection().await.expect("connection");
    let leases: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scheduler_leases")
        .fetch_one(connection.as_mut())
        .await
        .expect("leases");
    assert_eq!(leases, 0);
    assert!(!fixture.root.join("backups/restore-journal.json").exists());
}

#[tokio::test]
async fn exact_confirmation_is_required_before_pre_restore_or_maintenance() {
    let fixture = fixture("confirmation").await;
    let candidate = fixture
        .backups
        .create(BackupSource::Manual, &actor())
        .await
        .expect("candidate");

    let result = fixture
        .restore
        .restore(
            &candidate.id,
            "restore remindi",
            &actor(),
            RestoreFault::None,
        )
        .await;

    assert!(matches!(result, Err(BackupError::RestoreConfirmation)));
    assert_eq!(fixture.backups.list().await.expect("inventory").len(), 1);
    assert!(fixture.mcp.is_running());
    assert!(fixture.scheduler.is_running());
}

#[tokio::test]
async fn injected_failures_restore_the_verified_pre_restore_database() {
    for fault in [
        RestoreFault::BeforeSwap,
        RestoreFault::DuringReopen,
        RestoreFault::AfterSwap,
    ] {
        let fixture = fixture(&format!("{fault:?}")).await;
        let candidate = fixture
            .backups
            .create(BackupSource::Manual, &actor())
            .await
            .expect("candidate");
        set_setting(&fixture.database, 42).await;

        let result = fixture
            .restore
            .restore(&candidate.id, RESTORE_CONFIRMATION, &actor(), fault)
            .await;

        assert!(matches!(result, Err(BackupError::RestoreFailed)));
        assert_eq!(setting(&fixture.database).await, 42);
        assert!(fixture.mcp.is_running());
        assert!(fixture.scheduler.is_running());
        assert!(!fixture.root.join("backups/restore-journal.json").exists());
    }
}

#[tokio::test]
async fn unrelated_database_requests_fail_fast_while_maintenance_is_active() {
    let fixture = fixture("maintenance").await;
    let maintenance = fixture.database.begin_maintenance().await;

    let result = fixture.database.connection().await;

    assert!(matches!(result, Err(DatabaseError::MaintenanceActive)));
    drop(maintenance);
    fixture
        .database
        .connection()
        .await
        .expect("control returns after maintenance");
}

#[tokio::test]
async fn supported_version_one_restore_is_forward_migrated_and_revalidated() {
    let fixture = fixture("forward-migration").await;
    let version_one = fixture.root.join("version-one.sqlite3");
    version_one_database(&version_one).await;
    let candidate = fixture
        .backups
        .upload(
            stream::iter([Ok::<_, ()>(
                fs::read(&version_one).expect("v1 bytes").into(),
            )]),
            u64::MAX,
            &actor(),
        )
        .await
        .expect("v1 upload");

    fixture
        .restore
        .restore(
            &candidate.id,
            RESTORE_CONFIRMATION,
            &actor(),
            RestoreFault::None,
        )
        .await
        .expect("v1 restore");

    let mut connection = fixture.database.connection().await.expect("connection");
    let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM schema_migrations")
        .fetch_one(connection.as_mut())
        .await
        .expect("version");
    assert_eq!(version, 2);
}
