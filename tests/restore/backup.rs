use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use futures::stream;
use remindi::{
    admin::{
        AdminActor,
        backup::{BackupError, BackupManager, BackupSource},
    },
    clock::{FixedClock, UuidV7Generator},
    db::DatabaseManager,
};
use sha2::{Digest, Sha256};
use sqlx::{Connection, sqlite::SqliteConnectOptions};
use std::str::FromStr;
use time::macros::datetime;
use uuid::Uuid;

fn temporary_directory(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("remindi-backup-{label}-{}", Uuid::now_v7()));
    fs::create_dir_all(&path).expect("temporary directory is created");
    path
}

fn actor() -> AdminActor {
    AdminActor::new("web:owner", Some("req-backup".to_owned())).expect("valid actor")
}

async fn manager(label: &str) -> (Arc<DatabaseManager>, Arc<BackupManager>, PathBuf) {
    let root = temporary_directory(label);
    let database = Arc::new(
        DatabaseManager::open(root.join("remindi.db"))
            .await
            .expect("database opens"),
    );
    let manager = Arc::new(
        BackupManager::open(
            Arc::clone(&database),
            root.join("backups"),
            "owner",
            Arc::new(FixedClock::new(datetime!(2026-07-19 00:00 UTC))),
            Arc::new(UuidV7Generator),
        )
        .await
        .expect("backup manager opens"),
    );
    (database, manager, root)
}

fn digest(path: &Path) -> String {
    format!(
        "{:x}",
        Sha256::digest(fs::read(path).expect("backup bytes"))
    )
}

#[tokio::test]
async fn backup_creation_download_and_manifest_are_verified_and_protected() {
    let (database, manager, root) = manager("creation").await;

    let record = manager
        .create(BackupSource::Manual, &actor())
        .await
        .expect("manual backup");
    let (downloaded, path) = manager.download(&record.id).await.expect("download");
    manager
        .verify(&record.id, &actor())
        .await
        .expect("explicit verification");

    assert_eq!(downloaded.sha256, digest(&path));
    assert_eq!(downloaded.schema_version, 2);
    assert!(
        fs::metadata(path.with_extension("sqlite3.json"))
            .expect("manifest metadata")
            .len()
            > 0
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(root.join("backups"))
                .expect("directory metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(path)
                .expect("backup metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let mut connection = database.connection().await.expect("database connection");
    let verified_events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM admin_events WHERE event_type = 'backup_verified'",
    )
    .fetch_one(connection.as_mut())
    .await
    .expect("verification audit");
    assert_eq!(verified_events, 1);
}

#[tokio::test]
async fn upload_rejects_invalid_content_and_excess_size_without_registration() {
    let (_database, manager, root) = manager("upload-invalid").await;

    let invalid = manager
        .upload(
            stream::iter([Ok::<_, ()>(b"not sqlite".as_slice().into())]),
            1024,
            &actor(),
        )
        .await;
    let oversized = manager
        .upload(
            stream::iter([Ok::<_, ()>(vec![0_u8; 17].into())]),
            16,
            &actor(),
        )
        .await;

    assert!(matches!(invalid, Err(BackupError::Invalid)));
    assert!(matches!(oversized, Err(BackupError::LimitExceeded)));
    assert!(manager.list().await.expect("inventory").is_empty());
    assert!(
        fs::read_dir(root.join("backups"))
            .expect("backup directory")
            .all(|entry| !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".tmp-"))
    );
}

#[tokio::test]
async fn verified_database_upload_is_registered_with_its_digest() {
    let (_database, manager, _root) = manager("upload-valid").await;
    let source = manager
        .create(BackupSource::Manual, &actor())
        .await
        .expect("source backup");
    let (_, path) = manager.download(&source.id).await.expect("source download");
    let bytes = fs::read(path).expect("source bytes");

    let uploaded = manager
        .upload(
            stream::iter([Ok::<_, ()>(bytes.into())]),
            u64::MAX,
            &actor(),
        )
        .await
        .expect("valid upload");

    assert_eq!(uploaded.source, BackupSource::Upload);
    assert_eq!(
        manager
            .download(&uploaded.id)
            .await
            .expect("uploaded download")
            .0
            .sha256,
        uploaded.sha256
    );
}

#[tokio::test]
async fn reconciliation_rebuilds_only_verified_database_manifest_pairs_after_restart() {
    let (database, manager, root) = manager("reconcile").await;
    let record = manager
        .create(BackupSource::Manual, &actor())
        .await
        .expect("manual backup");
    {
        let mut connection = database.connection().await.expect("connection");
        sqlx::query("DELETE FROM backup_records WHERE id = ?")
            .bind(&record.id)
            .execute(connection.as_mut())
            .await
            .expect("remove inventory row");
    }
    drop(manager);

    let restarted = BackupManager::open(
        Arc::clone(&database),
        root.join("backups"),
        "owner",
        Arc::new(FixedClock::new(datetime!(2026-07-19 00:01 UTC))),
        Arc::new(UuidV7Generator),
    )
    .await
    .expect("restart reconciles");

    assert_eq!(restarted.list().await.expect("inventory").len(), 1);
}

#[tokio::test]
async fn reconciliation_rejects_a_manifest_that_is_not_the_database_sidecar() {
    let (database, manager, root) = manager("reconcile-sidecar").await;
    let record = manager
        .create(BackupSource::Manual, &actor())
        .await
        .expect("manual backup");
    let database_path = root.join("backups").join(&record.file_name);
    let sidecar = database_path.with_extension("sqlite3.json");
    fs::copy(&sidecar, root.join("backups/forged.json")).expect("copy manifest");
    fs::remove_file(sidecar).expect("remove matching sidecar");
    {
        let mut connection = database.connection().await.expect("connection");
        sqlx::query("DELETE FROM backup_records WHERE id = ?")
            .bind(&record.id)
            .execute(connection.as_mut())
            .await
            .expect("remove inventory row");
    }
    drop(manager);

    let restarted = BackupManager::open(
        Arc::clone(&database),
        root.join("backups"),
        "owner",
        Arc::new(FixedClock::new(datetime!(2026-07-19 00:01 UTC))),
        Arc::new(UuidV7Generator),
    )
    .await
    .expect("restart");

    assert!(restarted.list().await.expect("inventory").is_empty());
}

#[tokio::test]
async fn retention_expires_only_eligible_automatic_or_uploaded_backups() {
    let (database, manager, _root) = manager("retention").await;
    {
        let mut connection = database.connection().await.expect("connection");
        sqlx::query(
            "UPDATE runtime_settings SET value_json = '1' \
             WHERE setting_key = 'backups.retention_count'",
        )
        .execute(connection.as_mut())
        .await
        .expect("retention setting");
    }
    let manual = manager
        .create(BackupSource::Manual, &actor())
        .await
        .expect("manual backup");
    let first = manager
        .create(BackupSource::Automatic, &actor())
        .await
        .expect("first automatic");
    let second = manager
        .create(BackupSource::Automatic, &actor())
        .await
        .expect("second automatic");
    let inventory = manager.list().await.expect("inventory");

    assert_eq!(
        inventory
            .iter()
            .find(|record| record.id == manual.id)
            .expect("manual record")
            .status,
        "ready"
    );
    assert_eq!(
        inventory
            .iter()
            .filter(|record| {
                [first.id.as_str(), second.id.as_str()].contains(&record.id.as_str())
                    && record.status == "expired"
            })
            .count(),
        1
    );
}

#[tokio::test]
async fn interrupted_temporary_files_are_removed_on_restart() {
    let (database, manager, root) = manager("interrupted").await;
    let temporary = root.join("backups/.tmp-interrupted.sqlite3");
    fs::write(&temporary, b"partial").expect("temporary file");
    drop(manager);

    BackupManager::open(
        database,
        root.join("backups"),
        "owner",
        Arc::new(FixedClock::new(datetime!(2026-07-19 00:02 UTC))),
        Arc::new(UuidV7Generator),
    )
    .await
    .expect("restart cleanup");

    assert!(!temporary.exists());
}

#[tokio::test]
async fn upload_rejects_unsupported_schema_and_configured_owner_mismatch() {
    let (_database, manager, _root) = manager("upload-semantics").await;
    let source = manager
        .create(BackupSource::Manual, &actor())
        .await
        .expect("source backup");
    let (_, path) = manager.download(&source.id).await.expect("source download");
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
        .expect("candidate options");
    let mut connection = sqlx::SqliteConnection::connect_with(&options)
        .await
        .expect("candidate opens");
    sqlx::query(
        "INSERT INTO remindi(\
           id, owner_id, project_id, message, state, trigger_type, trigger_spec_json, \
           next_fire_at, created_at, updated_at\
         ) VALUES (?, 'different-owner', 'project', 'private', 'scheduled', \
                   'at_time', '{}', ?, ?, ?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind("2026-07-20T00:00:00.000Z")
    .bind("2026-07-19T00:00:00.000Z")
    .bind("2026-07-19T00:00:00.000Z")
    .execute(&mut connection)
    .await
    .expect("owner-mismatched row");
    connection.close().await.expect("candidate closes");
    let owner_mismatch = manager
        .upload(
            stream::iter([Ok::<_, ()>(
                fs::read(&path).expect("candidate bytes").into(),
            )]),
            u64::MAX,
            &actor(),
        )
        .await;

    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
        .expect("candidate options");
    let mut connection = sqlx::SqliteConnection::connect_with(&options)
        .await
        .expect("candidate reopens");
    sqlx::query("DELETE FROM remindi")
        .execute(&mut connection)
        .await
        .expect("owner row removed");
    sqlx::query("UPDATE schema_migrations SET version = 99 WHERE version = 2")
        .execute(&mut connection)
        .await
        .expect("unsupported schema marker");
    connection.close().await.expect("candidate closes");
    let unsupported_schema = manager
        .upload(
            stream::iter([Ok::<_, ()>(fs::read(path).expect("candidate bytes").into())]),
            u64::MAX,
            &actor(),
        )
        .await;

    assert!(matches!(owner_mismatch, Err(BackupError::Invalid)));
    assert!(matches!(unsupported_schema, Err(BackupError::Invalid)));
}

#[tokio::test]
async fn upload_rejects_cross_table_application_invariant_violation() {
    let (_database, manager, _root) = manager("upload-invariant").await;
    let source = manager
        .create(BackupSource::Manual, &actor())
        .await
        .expect("source backup");
    let (_, path) = manager.download(&source.id).await.expect("source download");
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
        .expect("candidate options");
    let mut connection = sqlx::SqliteConnection::connect_with(&options)
        .await
        .expect("candidate opens");
    sqlx::query(
        "INSERT INTO remindi(\
           id, owner_id, project_id, message, state, trigger_type, trigger_spec_json, \
           next_fire_at, created_at, updated_at, completed_at\
         ) VALUES (?, 'owner', 'project', 'private', 'completed', \
                   'at_time', '{}', ?, ?, ?, ?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind("2026-07-19T00:00:00.000Z")
    .bind("2026-07-19T00:00:00.000Z")
    .bind("2026-07-19T00:00:00.000Z")
    .bind("2026-07-19T00:00:00.000Z")
    .execute(&mut connection)
    .await
    .expect("completed item without evidence");
    connection.close().await.expect("candidate closes");

    let result = manager
        .upload(
            stream::iter([Ok::<_, ()>(fs::read(path).expect("candidate bytes").into())]),
            u64::MAX,
            &actor(),
        )
        .await;

    assert!(matches!(result, Err(BackupError::Invalid)));
}
