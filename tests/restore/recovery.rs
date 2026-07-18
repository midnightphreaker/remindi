use std::{fs, path::PathBuf, sync::Arc};

use remindi::{
    admin::{
        AdminActor,
        backup::{BackupManager, BackupSource, RestoreManager, RestorePhase},
    },
    clock::{FixedClock, UuidV7Generator},
    db::DatabaseManager,
};
use serde_json::json;
use time::macros::datetime;
use uuid::Uuid;

fn actor() -> AdminActor {
    AdminActor::new("web:owner", Some("req-recovery".to_owned())).expect("actor")
}

async fn interrupted(phase: RestorePhase) -> (PathBuf, String) {
    let root = std::env::temp_dir().join(format!("remindi-recovery-{phase:?}-{}", Uuid::now_v7()));
    fs::create_dir_all(&root).expect("root");
    let live = root.join("remindi.db");
    let database = Arc::new(DatabaseManager::open(&live).await.expect("database"));
    let backups = BackupManager::open(
        Arc::clone(&database),
        root.join("backups"),
        "owner",
        Arc::new(FixedClock::new(datetime!(2026-07-19 02:00 UTC))),
        Arc::new(UuidV7Generator),
    )
    .await
    .expect("backups");
    let pre = backups
        .create(BackupSource::PreRestore, &actor())
        .await
        .expect("pre restore");
    {
        let mut transaction = database.begin_immediate().await.expect("transaction");
        sqlx::query(
            "UPDATE runtime_settings SET value_json = '42' \
             WHERE setting_key = 'remindi.default_overdue_seconds'",
        )
        .execute(transaction.as_mut())
        .await
        .expect("change live");
        transaction.commit().await.expect("commit");
    }
    drop(backups);
    Arc::try_unwrap(database)
        .expect("database owner")
        .close()
        .await
        .expect("close");
    let journal = json!({
        "operation_id": Uuid::now_v7().to_string(),
        "phase": phase,
        "candidate_file": pre.file_name,
        "pre_restore_file": pre.file_name,
    });
    fs::write(
        root.join("backups/restore-journal.json"),
        serde_json::to_vec(&journal).expect("journal"),
    )
    .expect("write journal");
    (root, pre.file_name)
}

async fn value(path: &std::path::Path) -> i64 {
    let database = DatabaseManager::open(path)
        .await
        .expect("recovered database");
    let mut connection = database.connection().await.expect("connection");
    sqlx::query_scalar(
        "SELECT CAST(value_json AS INTEGER) FROM runtime_settings \
         WHERE setting_key = 'remindi.default_overdue_seconds'",
    )
    .fetch_one(connection.as_mut())
    .await
    .expect("value")
}

#[tokio::test]
async fn startup_reconciles_every_journal_phase_to_a_complete_database() {
    for phase in [
        RestorePhase::Requested,
        RestorePhase::CandidateVerified,
        RestorePhase::PreRestoreBackupVerified,
        RestorePhase::WorkloadsQuiesced,
        RestorePhase::LiveReplaced,
        RestorePhase::ReplacementVerified,
        RestorePhase::WorkloadsRestarted,
        RestorePhase::RollbackStarted,
        RestorePhase::PreRestoreReinstalled,
        RestorePhase::RollbackVerified,
        RestorePhase::WorkloadsRestartedOrHeld,
        RestorePhase::Failed,
    ] {
        let (root, _) = interrupted(phase).await;
        let recovered = RestoreManager::recover_interrupted(
            &root.join("remindi.db"),
            &root.join("backups"),
            "owner",
            datetime!(2026-07-19 02:01 UTC),
        )
        .await
        .expect("recovery succeeds");

        assert!(recovered);
        let expected = if matches!(
            phase,
            RestorePhase::Requested | RestorePhase::CandidateVerified
        ) {
            42
        } else {
            0
        };
        assert_eq!(value(&root.join("remindi.db")).await, expected);
        assert!(!root.join("backups/restore-journal.json").exists());
    }
}

#[tokio::test]
async fn completed_journal_is_removed_without_overwriting_the_live_database() {
    let (root, _) = interrupted(RestorePhase::Succeeded).await;

    RestoreManager::recover_interrupted(
        &root.join("remindi.db"),
        &root.join("backups"),
        "owner",
        datetime!(2026-07-19 02:01 UTC),
    )
    .await
    .expect("completed journal cleanup");

    assert_eq!(value(&root.join("remindi.db")).await, 42);
}

#[tokio::test]
async fn recovery_rejects_an_unsafe_operation_identifier_without_touching_live_state() {
    let (root, pre_restore_file) = interrupted(RestorePhase::LiveReplaced).await;
    fs::write(
        root.join("backups/restore-journal.json"),
        serde_json::to_vec(&json!({
            "operation_id": "../../escape",
            "phase": "live_replaced",
            "candidate_file": pre_restore_file.clone(),
            "pre_restore_file": pre_restore_file,
        }))
        .expect("journal"),
    )
    .expect("unsafe journal");

    let result = RestoreManager::recover_interrupted(
        &root.join("remindi.db"),
        &root.join("backups"),
        "owner",
        datetime!(2026-07-19 02:01 UTC),
    )
    .await;

    assert!(result.is_err());
    assert_eq!(value(&root.join("remindi.db")).await, 42);
}
