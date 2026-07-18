//! Verified SQLite backup inventory and retention.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use axum::body::Bytes;
use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    Connection, Row,
    sqlite::{SqliteConnectOptions, SqliteConnection},
};
use thiserror::Error;
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Mutex,
};
use uuid::Uuid;

use crate::{
    clock::{Clock, IdGenerator},
    db::DatabaseManager,
    remindi::canonical_timestamp,
};

use super::{
    AdminActor, audit,
    workloads::{WorkloadController, WorkloadError},
};

const CURRENT_SCHEMA_VERSION: i64 = 2;
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";
const DETAIL_LIMIT: usize = 1_024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupSource {
    Manual,
    Automatic,
    Upload,
    PreRestore,
}

impl BackupSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Automatic => "automatic",
            Self::Upload => "upload",
            Self::PreRestore => "pre_restore",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BackupRecord {
    pub id: String,
    pub file_name: String,
    pub source: BackupSource,
    pub status: String,
    pub sha256: String,
    pub size_bytes: i64,
    pub schema_version: i64,
    pub created_at: String,
    pub verified_at: Option<String>,
    pub created_by: String,
    pub details: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct BackupManifest {
    id: String,
    file_name: String,
    source: BackupSource,
    sha256: String,
    size_bytes: i64,
    schema_version: i64,
    created_at: String,
    verified_at: String,
    created_by: String,
    details: Value,
}

#[derive(Debug, Error)]
pub enum BackupError {
    #[error("backup input is invalid")]
    Invalid,
    #[error("backup input exceeded its configured limit")]
    LimitExceeded,
    #[error("backup was not found")]
    NotFound,
    #[error("backup persistence failed")]
    Database,
    #[error("backup filesystem operation failed")]
    Io,
    #[error("restore confirmation was invalid")]
    RestoreConfirmation,
    #[error("restore failed and rollback was attempted")]
    RestoreFailed,
    #[error("restore workload transition failed")]
    Workload,
}

/// Exact typed phrase required by the guarded restore contract.
pub const RESTORE_CONFIRMATION: &str = "RESTORE REMINDI";

/// Durable restore journal phases from DESIGN 18.4.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RestorePhase {
    Requested,
    CandidateVerified,
    PreRestoreBackupVerified,
    WorkloadsQuiesced,
    LiveReplaced,
    ReplacementVerified,
    WorkloadsRestarted,
    Succeeded,
    RollbackStarted,
    PreRestoreReinstalled,
    RollbackVerified,
    WorkloadsRestartedOrHeld,
    Failed,
}

/// Deterministic failure seam used by restore integration tests.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RestoreFault {
    #[default]
    None,
    BeforeSwap,
    DuringReopen,
    AfterSwap,
}

/// Redacted result of a guarded restore.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RestoreOutcome {
    pub operation_id: String,
    pub backup_id: String,
    pub restored: bool,
    pub rolled_back: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RestoreJournal {
    operation_id: String,
    phase: RestorePhase,
    candidate_file: String,
    pre_restore_file: String,
}

pub struct BackupManager {
    database: Arc<DatabaseManager>,
    directory: PathBuf,
    owner_id: String,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    mutation: Mutex<()>,
}

/// Coordinates one guarded restore at a time without stopping the control plane.
pub struct RestoreManager {
    database: Arc<DatabaseManager>,
    backups: Arc<BackupManager>,
    workloads: Arc<WorkloadController>,
    operation: Mutex<()>,
}

impl BackupManager {
    pub async fn open(
        database: Arc<DatabaseManager>,
        directory: impl Into<PathBuf>,
        owner_id: impl Into<String>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Result<Self, BackupError> {
        let manager = Self {
            database,
            directory: directory.into(),
            owner_id: owner_id.into(),
            clock,
            ids,
            mutation: Mutex::new(()),
        };
        manager.prepare_directory().await?;
        manager.reconcile().await?;
        Ok(manager)
    }

    pub async fn create(
        &self,
        source: BackupSource,
        actor: &AdminActor,
    ) -> Result<BackupRecord, BackupError> {
        if source == BackupSource::Upload {
            return Err(BackupError::Invalid);
        }
        let _guard = self.mutation.lock().await;
        let identity = self.identity(source, actor)?;
        let temporary = self.temporary_database_path(&identity.id);
        let final_path = self.directory.join(&identity.file_name);
        let temporary_string = temporary.to_string_lossy().into_owned();

        let mut connection = self
            .database
            .connection()
            .await
            .map_err(|_| BackupError::Database)?;
        sqlx::query("VACUUM INTO ?")
            .bind(&temporary_string)
            .execute(connection.as_mut())
            .await
            .map_err(|_| BackupError::Database)?;
        drop(connection);

        protect_file(&temporary).await?;
        sync_file(&temporary).await?;
        let verified = self.verify_database(&temporary).await?;
        let manifest = identity.finish(verified);
        self.publish(&temporary, &final_path, &manifest).await?;
        let record = self.register(&manifest, "backup_created", actor).await?;
        self.apply_retention(actor).await?;
        Ok(record)
    }

    pub async fn upload<S, E>(
        &self,
        mut chunks: S,
        maximum_bytes: u64,
        actor: &AdminActor,
    ) -> Result<BackupRecord, BackupError>
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin,
    {
        let _guard = self.mutation.lock().await;
        let identity = self.identity(BackupSource::Upload, actor)?;
        let temporary = self.temporary_database_path(&identity.id);
        let final_path = self.directory.join(&identity.file_name);
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .await
            .map_err(|_| BackupError::Io)?;
        protect_file(&temporary).await?;

        let mut written = 0_u64;
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|_| BackupError::Invalid)?;
            written = written
                .checked_add(u64::try_from(chunk.len()).map_err(|_| BackupError::LimitExceeded)?)
                .ok_or(BackupError::LimitExceeded)?;
            if written > maximum_bytes {
                drop(file);
                remove_if_exists(&temporary).await;
                return Err(BackupError::LimitExceeded);
            }
            file.write_all(&chunk).await.map_err(|_| BackupError::Io)?;
        }
        file.sync_all().await.map_err(|_| BackupError::Io)?;
        drop(file);

        let verified = match self.verify_database(&temporary).await {
            Ok(verified) => verified,
            Err(error) => {
                remove_if_exists(&temporary).await;
                return Err(error);
            }
        };
        let manifest = identity.finish(verified);
        self.publish(&temporary, &final_path, &manifest).await?;
        let record = self.register(&manifest, "backup_uploaded", actor).await?;
        self.apply_retention(actor).await?;
        Ok(record)
    }

    pub async fn list(&self) -> Result<Vec<BackupRecord>, BackupError> {
        let mut connection = self
            .database
            .connection()
            .await
            .map_err(|_| BackupError::Database)?;
        let rows = sqlx::query(
            "SELECT id, file_name, source, status, sha256, size_bytes, \
                    schema_version, created_at, verified_at, created_by, details_json \
             FROM backup_records ORDER BY created_at DESC, id DESC",
        )
        .fetch_all(connection.as_mut())
        .await
        .map_err(|_| BackupError::Database)?;
        rows.into_iter().map(record_from_row).collect()
    }

    pub async fn download(&self, id: &str) -> Result<(BackupRecord, PathBuf), BackupError> {
        let record = self
            .list()
            .await?
            .into_iter()
            .find(|record| record.id == id && record.status != "expired")
            .ok_or(BackupError::NotFound)?;
        if !safe_file_name(&record.file_name) {
            return Err(BackupError::Invalid);
        }
        let path = self.directory.join(&record.file_name);
        let manifest = read_manifest(&manifest_path(&path)).await?;
        let verified = self.verify_database(&path).await?;
        if manifest.id != record.id
            || manifest.sha256 != record.sha256
            || verified.sha256 != record.sha256
            || verified.size_bytes != record.size_bytes
        {
            return Err(BackupError::Invalid);
        }
        Ok((record, path))
    }

    pub async fn verify(&self, id: &str, actor: &AdminActor) -> Result<BackupRecord, BackupError> {
        let (record, _) = self.download(id).await?;
        audit::append(
            &self.database,
            self.clock.as_ref(),
            self.ids.as_ref(),
            "backup_verified",
            actor,
            "succeeded",
            &json!({"backup_id": record.id}),
        )
        .await
        .map_err(|_| BackupError::Database)?;
        Ok(record)
    }

    pub async fn reconcile(&self) -> Result<usize, BackupError> {
        self.prepare_directory().await?;
        let mut entries = fs::read_dir(&self.directory)
            .await
            .map_err(|_| BackupError::Io)?;
        let mut reconciled = 0;
        while let Some(entry) = entries.next_entry().await.map_err(|_| BackupError::Io)? {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(OsStr::to_str) else {
                continue;
            };
            if is_temporary_name(name) {
                remove_if_exists(&path).await;
                continue;
            }
            if path.extension() != Some(OsStr::new("json")) {
                continue;
            }
            let manifest = match read_manifest(&path).await {
                Ok(manifest) => manifest,
                Err(_) => continue,
            };
            let database_path = self.directory.join(&manifest.file_name);
            if path != manifest_path(&database_path) {
                continue;
            }
            let verified = match self.verify_database(&database_path).await {
                Ok(verified) => verified,
                Err(_) => continue,
            };
            if manifest.sha256 != verified.sha256
                || manifest.size_bytes != verified.size_bytes
                || manifest.schema_version != verified.schema_version
            {
                continue;
            }
            let inserted = self.insert_if_missing(&manifest).await?;
            reconciled += usize::from(inserted);
        }
        Ok(reconciled)
    }

    pub async fn apply_retention(&self, actor: &AdminActor) -> Result<usize, BackupError> {
        let mut connection = self
            .database
            .connection()
            .await
            .map_err(|_| BackupError::Database)?;
        let retention: i64 = sqlx::query_scalar(
            "SELECT CAST(value_json AS INTEGER) FROM runtime_settings \
             WHERE setting_key = 'backups.retention_count'",
        )
        .fetch_one(connection.as_mut())
        .await
        .map_err(|_| BackupError::Database)?;
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT id, file_name FROM backup_records \
             WHERE status = 'ready' AND source IN ('automatic', 'upload') \
             ORDER BY created_at DESC, id DESC LIMIT -1 OFFSET ?",
        )
        .bind(retention)
        .fetch_all(connection.as_mut())
        .await
        .map_err(|_| BackupError::Database)?;
        drop(connection);

        for (id, file_name) in &rows {
            let mut transaction = self
                .database
                .begin_immediate()
                .await
                .map_err(|_| BackupError::Database)?;
            sqlx::query(
                "UPDATE backup_records SET status = 'expired' WHERE id = ? AND status = 'ready'",
            )
            .bind(id)
            .execute(transaction.as_mut())
            .await
            .map_err(|_| BackupError::Database)?;
            audit::insert(
                &mut transaction,
                self.clock.as_ref(),
                self.ids.as_ref(),
                "backup_expired",
                actor,
                "succeeded",
                &json!({"backup_id": id}),
            )
            .await
            .map_err(|_| BackupError::Database)?;
            transaction
                .commit()
                .await
                .map_err(|_| BackupError::Database)?;
            let path = self.directory.join(file_name);
            remove_if_exists(&path).await;
            remove_if_exists(&manifest_path(&path)).await;
        }
        if !rows.is_empty() {
            sync_directory(&self.directory).await?;
        }
        Ok(rows.len())
    }

    fn identity(
        &self,
        source: BackupSource,
        actor: &AdminActor,
    ) -> Result<PendingBackup, BackupError> {
        let id = self.ids.next_id().to_string();
        let created_at =
            canonical_timestamp(self.clock.now()).map_err(|_| BackupError::Database)?;
        Ok(PendingBackup {
            file_name: format!("remindi-{id}.sqlite3"),
            id,
            source,
            created_at,
            created_by: actor.actor_id().to_owned(),
        })
    }

    fn temporary_database_path(&self, id: &str) -> PathBuf {
        self.directory.join(format!(".tmp-{id}.sqlite3"))
    }

    async fn prepare_directory(&self) -> Result<(), BackupError> {
        fs::create_dir_all(&self.directory)
            .await
            .map_err(|_| BackupError::Io)?;
        protect_directory(&self.directory).await?;
        remove_if_exists(&self.directory.join(".restore-journal.tmp")).await;
        Ok(())
    }

    async fn verify_database(&self, path: &Path) -> Result<VerifiedFile, BackupError> {
        verify_database_at(path, &self.owner_id, self.clock.now()).await
    }

    async fn publish(
        &self,
        temporary: &Path,
        final_path: &Path,
        manifest: &BackupManifest,
    ) -> Result<(), BackupError> {
        let temporary_manifest = self
            .directory
            .join(format!(".tmp-{}.manifest.json", manifest.id));
        let manifest_bytes = serde_json::to_vec(manifest).map_err(|_| BackupError::Database)?;
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary_manifest)
            .await
            .map_err(|_| BackupError::Io)?;
        file.write_all(&manifest_bytes)
            .await
            .map_err(|_| BackupError::Io)?;
        file.sync_all().await.map_err(|_| BackupError::Io)?;
        drop(file);
        protect_file(&temporary_manifest).await?;
        sync_file(&temporary_manifest).await?;
        fs::rename(temporary, final_path)
            .await
            .map_err(|_| BackupError::Io)?;
        fs::rename(&temporary_manifest, manifest_path(final_path))
            .await
            .map_err(|_| BackupError::Io)?;
        sync_directory(&self.directory).await
    }

    async fn register(
        &self,
        manifest: &BackupManifest,
        event_type: &'static str,
        actor: &AdminActor,
    ) -> Result<BackupRecord, BackupError> {
        let mut transaction = self
            .database
            .begin_immediate()
            .await
            .map_err(|_| BackupError::Database)?;
        insert_manifest(&mut transaction, manifest)
            .await
            .map_err(|_| BackupError::Database)?;
        audit::insert(
            &mut transaction,
            self.clock.as_ref(),
            self.ids.as_ref(),
            event_type,
            actor,
            "succeeded",
            &json!({"backup_id": manifest.id}),
        )
        .await
        .map_err(|_| BackupError::Database)?;
        transaction
            .commit()
            .await
            .map_err(|_| BackupError::Database)?;
        Ok(manifest.clone().into())
    }

    async fn insert_if_missing(&self, manifest: &BackupManifest) -> Result<bool, BackupError> {
        let mut transaction = self
            .database
            .begin_immediate()
            .await
            .map_err(|_| BackupError::Database)?;
        let result = insert_manifest(&mut transaction, manifest)
            .await
            .map_err(|_| BackupError::Database)?;
        transaction
            .commit()
            .await
            .map_err(|_| BackupError::Database)?;
        Ok(result.rows_affected() == 1)
    }
}

async fn verify_database_at(
    path: &Path,
    owner_id: &str,
    now: time::OffsetDateTime,
) -> Result<VerifiedFile, BackupError> {
    verify_header(path).await?;
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
        .map_err(|_| BackupError::Invalid)?
        .read_only(true)
        .create_if_missing(false);
    let mut connection = SqliteConnection::connect_with(&options)
        .await
        .map_err(|_| BackupError::Invalid)?;
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&mut connection)
        .await
        .map_err(|_| BackupError::Invalid)?;
    if integrity != "ok" {
        return Err(BackupError::Invalid);
    }
    let schema_version: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(version), 0) FROM schema_migrations")
            .fetch_one(&mut connection)
            .await
            .map_err(|_| BackupError::Invalid)?;
    if !(1..=CURRENT_SCHEMA_VERSION).contains(&schema_version) {
        return Err(BackupError::Invalid);
    }
    let wrong_owner: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM remindi WHERE owner_id <> ?")
        .bind(owner_id)
        .fetch_one(&mut connection)
        .await
        .map_err(|_| BackupError::Invalid)?;
    let invariant_violations: i64 = sqlx::query_scalar(
        "SELECT \
               (SELECT COUNT(*) FROM remindi r WHERE \
                  (r.state = 'completed') <> \
                  (SELECT COUNT(*) = 1 FROM completion_evidence e WHERE e.remindi_id = r.id)) + \
               (SELECT COUNT(*) FROM remindi WHERE \
                  (snooze_until IS NULL) <> (snoozed_from_state IS NULL))",
    )
    .fetch_one(&mut connection)
    .await
    .map_err(|_| BackupError::Invalid)?;
    connection.close().await.map_err(|_| BackupError::Invalid)?;
    if wrong_owner != 0 || invariant_violations != 0 {
        return Err(BackupError::Invalid);
    }
    hash_file(path, schema_version, now).await
}

impl RestoreManager {
    #[must_use]
    pub fn new(
        database: Arc<DatabaseManager>,
        backups: Arc<BackupManager>,
        workloads: Arc<WorkloadController>,
    ) -> Self {
        Self {
            database,
            backups,
            workloads,
            operation: Mutex::new(()),
        }
    }

    /// Restores one previously verified backup under exclusive maintenance.
    pub async fn restore(
        &self,
        backup_id: &str,
        confirmation: &str,
        actor: &AdminActor,
        fault: RestoreFault,
    ) -> Result<RestoreOutcome, BackupError> {
        if confirmation != RESTORE_CONFIRMATION {
            return Err(BackupError::RestoreConfirmation);
        }
        let _operation = self.operation.lock().await;
        let requested = self
            .backups
            .list()
            .await?
            .into_iter()
            .find(|record| record.id == backup_id && record.status != "expired")
            .ok_or(BackupError::NotFound)?;
        if !safe_file_name(&requested.file_name) {
            return Err(BackupError::Invalid);
        }
        let mut journal = RestoreJournal {
            operation_id: self.backups.ids.next_id().to_string(),
            phase: RestorePhase::Requested,
            candidate_file: requested.file_name,
            pre_restore_file: String::new(),
        };
        self.write_journal(&journal).await?;
        let (candidate, candidate_path) = match self.backups.download(backup_id).await {
            Ok(candidate) => candidate,
            Err(error) => {
                self.remove_journal().await?;
                return Err(error);
            }
        };
        journal.phase = RestorePhase::CandidateVerified;
        self.write_journal(&journal).await?;
        let pre_restore = match self.backups.create(BackupSource::PreRestore, actor).await {
            Ok(pre_restore) => pre_restore,
            Err(error) => {
                self.remove_journal().await?;
                return Err(error);
            }
        };
        let (_, pre_restore_path) = match self.backups.download(&pre_restore.id).await {
            Ok(pre_restore) => pre_restore,
            Err(error) => {
                self.remove_journal().await?;
                return Err(error);
            }
        };
        journal.pre_restore_file = pre_restore.file_name.clone();
        journal.phase = RestorePhase::PreRestoreBackupVerified;
        self.write_journal(&journal).await?;

        let workloads = match self.workloads.quiesce_for_maintenance().await {
            Ok(workloads) => workloads,
            Err(error) => {
                self.remove_journal().await?;
                return Err(map_workload_error(error));
            }
        };

        let mut database = self.database.begin_maintenance().await;
        let operation = async {
            journal.phase = RestorePhase::WorkloadsQuiesced;
            self.write_journal(&journal).await?;
            database
                .checkpoint_and_close()
                .await
                .map_err(|_| BackupError::RestoreFailed)?;
            if fault == RestoreFault::BeforeSwap {
                return Err(BackupError::RestoreFailed);
            }
            install_database(&candidate_path, self.database.path(), &journal.operation_id).await?;
            journal.phase = RestorePhase::LiveReplaced;
            self.write_journal(&journal).await?;
            if fault == RestoreFault::DuringReopen {
                return Err(BackupError::RestoreFailed);
            }
            database
                .reopen()
                .await
                .map_err(|_| BackupError::RestoreFailed)?;
            journal.phase = RestorePhase::ReplacementVerified;
            self.write_journal(&journal).await?;
            if fault == RestoreFault::AfterSwap {
                return Err(BackupError::RestoreFailed);
            }
            database
                .clear_scheduler_leases()
                .await
                .map_err(|_| BackupError::RestoreFailed)?;
            Ok::<(), BackupError>(())
        }
        .await;

        if operation.is_ok() {
            drop(database);
            let completion = async {
                self.backups.reconcile().await?;
                workloads
                    .restart_from_persisted()
                    .await
                    .map_err(map_workload_error)?;
                Ok::<(), BackupError>(())
            }
            .await;
            if completion.is_err() {
                let _ = workloads.quiesce_again().await;
                journal.phase = RestorePhase::RollbackStarted;
                let _ = self.write_journal(&journal).await;
                let mut rollback_database = self.database.begin_maintenance().await;
                let _ = rollback_database.checkpoint_and_close().await;
                install_database(
                    &pre_restore_path,
                    self.database.path(),
                    &journal.operation_id,
                )
                .await?;
                journal.phase = RestorePhase::PreRestoreReinstalled;
                let _ = self.write_journal(&journal).await;
                rollback_database
                    .reopen()
                    .await
                    .map_err(|_| BackupError::RestoreFailed)?;
                journal.phase = RestorePhase::RollbackVerified;
                let _ = self.write_journal(&journal).await;
                rollback_database
                    .clear_scheduler_leases()
                    .await
                    .map_err(|_| BackupError::RestoreFailed)?;
                drop(rollback_database);
                let restart = workloads.restart_from_persisted().await;
                journal.phase = RestorePhase::WorkloadsRestartedOrHeld;
                let _ = self.write_journal(&journal).await;
                audit::append(
                    &self.database,
                    self.backups.clock.as_ref(),
                    self.backups.ids.as_ref(),
                    "restore_failed",
                    actor,
                    "failed",
                    &json!({"backup_id": candidate.id, "operation_id": journal.operation_id}),
                )
                .await
                .map_err(|_| BackupError::Database)?;
                journal.phase = RestorePhase::Failed;
                let _ = self.write_journal(&journal).await;
                self.remove_journal().await?;
                restart.map_err(map_workload_error)?;
                return Err(BackupError::RestoreFailed);
            }
            journal.phase = RestorePhase::WorkloadsRestarted;
            self.write_journal(&journal).await?;
            audit::append(
                &self.database,
                self.backups.clock.as_ref(),
                self.backups.ids.as_ref(),
                "restore_succeeded",
                actor,
                "succeeded",
                &json!({"backup_id": candidate.id, "operation_id": journal.operation_id}),
            )
            .await
            .map_err(|_| BackupError::Database)?;
            journal.phase = RestorePhase::Succeeded;
            self.write_journal(&journal).await?;
            self.remove_journal().await?;
            return Ok(RestoreOutcome {
                operation_id: journal.operation_id,
                backup_id: candidate.id,
                restored: true,
                rolled_back: false,
            });
        }

        journal.phase = RestorePhase::RollbackStarted;
        let _ = self.write_journal(&journal).await;
        let _ = database.checkpoint_and_close().await;
        install_database(
            &pre_restore_path,
            self.database.path(),
            &journal.operation_id,
        )
        .await?;
        journal.phase = RestorePhase::PreRestoreReinstalled;
        let _ = self.write_journal(&journal).await;
        database
            .reopen()
            .await
            .map_err(|_| BackupError::RestoreFailed)?;
        journal.phase = RestorePhase::RollbackVerified;
        let _ = self.write_journal(&journal).await;
        database
            .clear_scheduler_leases()
            .await
            .map_err(|_| BackupError::RestoreFailed)?;
        drop(database);
        let restart = workloads.restart_from_persisted().await;
        journal.phase = RestorePhase::WorkloadsRestartedOrHeld;
        let _ = self.write_journal(&journal).await;
        audit::append(
            &self.database,
            self.backups.clock.as_ref(),
            self.backups.ids.as_ref(),
            "restore_failed",
            actor,
            "failed",
            &json!({"backup_id": candidate.id, "operation_id": journal.operation_id}),
        )
        .await
        .map_err(|_| BackupError::Database)?;
        journal.phase = RestorePhase::Failed;
        let _ = self.write_journal(&journal).await;
        self.remove_journal().await?;
        restart.map_err(map_workload_error)?;
        Err(BackupError::RestoreFailed)
    }

    /// Repairs an interrupted restore before the live SQLx pool is opened.
    pub async fn recover_interrupted(
        live_path: &Path,
        backup_directory: &Path,
        owner_id: &str,
        now: time::OffsetDateTime,
    ) -> Result<bool, BackupError> {
        let journal_path = backup_directory.join("restore-journal.json");
        let bytes = match fs::read(&journal_path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err(BackupError::Io),
        };
        if bytes.len() > 16 * 1024 {
            return Err(BackupError::Invalid);
        }
        let journal: RestoreJournal =
            serde_json::from_slice(&bytes).map_err(|_| BackupError::Invalid)?;
        if Uuid::parse_str(&journal.operation_id).is_err()
            || !safe_file_name(&journal.candidate_file)
            || (!matches!(
                journal.phase,
                RestorePhase::Requested | RestorePhase::CandidateVerified
            ) && !safe_file_name(&journal.pre_restore_file))
        {
            return Err(BackupError::Invalid);
        }
        if matches!(
            journal.phase,
            RestorePhase::Requested | RestorePhase::CandidateVerified
        ) {
            verify_database_at(live_path, owner_id, now).await?;
        } else if journal.phase != RestorePhase::Succeeded {
            let pre_restore = backup_directory.join(&journal.pre_restore_file);
            verify_database_at(&pre_restore, owner_id, now).await?;
            install_database(&pre_restore, live_path, &journal.operation_id).await?;
            verify_database_at(live_path, owner_id, now).await?;
        }
        fs::remove_file(&journal_path)
            .await
            .map_err(|_| BackupError::Io)?;
        sync_directory(backup_directory).await?;
        Ok(true)
    }

    fn journal_path(&self) -> PathBuf {
        self.backups.directory.join("restore-journal.json")
    }

    async fn write_journal(&self, journal: &RestoreJournal) -> Result<(), BackupError> {
        let path = self.journal_path();
        let temporary = self.backups.directory.join(".restore-journal.tmp");
        let bytes = serde_json::to_vec(journal).map_err(|_| BackupError::Database)?;
        let mut file = fs::File::create(&temporary)
            .await
            .map_err(|_| BackupError::Io)?;
        file.write_all(&bytes).await.map_err(|_| BackupError::Io)?;
        file.sync_all().await.map_err(|_| BackupError::Io)?;
        drop(file);
        protect_file(&temporary).await?;
        fs::rename(&temporary, &path)
            .await
            .map_err(|_| BackupError::Io)?;
        sync_directory(&self.backups.directory).await
    }

    async fn remove_journal(&self) -> Result<(), BackupError> {
        remove_if_exists(&self.journal_path()).await;
        sync_directory(&self.backups.directory).await
    }
}

fn map_workload_error(_error: WorkloadError) -> BackupError {
    BackupError::Workload
}

struct PendingBackup {
    id: String,
    file_name: String,
    source: BackupSource,
    created_at: String,
    created_by: String,
}

impl PendingBackup {
    fn finish(self, verified: VerifiedFile) -> BackupManifest {
        BackupManifest {
            id: self.id,
            file_name: self.file_name,
            source: self.source,
            sha256: verified.sha256,
            size_bytes: verified.size_bytes,
            schema_version: verified.schema_version,
            created_at: self.created_at,
            verified_at: verified.verified_at,
            created_by: self.created_by,
            details: json!({}),
        }
    }
}

struct VerifiedFile {
    sha256: String,
    size_bytes: i64,
    schema_version: i64,
    verified_at: String,
}

impl From<BackupManifest> for BackupRecord {
    fn from(manifest: BackupManifest) -> Self {
        Self {
            id: manifest.id,
            file_name: manifest.file_name,
            source: manifest.source,
            status: "ready".to_owned(),
            sha256: manifest.sha256,
            size_bytes: manifest.size_bytes,
            schema_version: manifest.schema_version,
            created_at: manifest.created_at,
            verified_at: Some(manifest.verified_at),
            created_by: manifest.created_by,
            details: manifest.details,
        }
    }
}

async fn insert_manifest(
    transaction: &mut crate::db::ImmediateTransaction,
    manifest: &BackupManifest,
) -> Result<sqlx::sqlite::SqliteQueryResult, sqlx::Error> {
    sqlx::query(
        "INSERT OR IGNORE INTO backup_records(\
             id, file_name, source, status, sha256, size_bytes, schema_version, \
             created_at, verified_at, created_by, details_json\
         ) VALUES (?, ?, ?, 'ready', ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&manifest.id)
    .bind(&manifest.file_name)
    .bind(manifest.source.as_str())
    .bind(&manifest.sha256)
    .bind(manifest.size_bytes)
    .bind(manifest.schema_version)
    .bind(&manifest.created_at)
    .bind(&manifest.verified_at)
    .bind(&manifest.created_by)
    .bind(
        serde_json::to_string(&manifest.details)
            .map_err(|error| sqlx::Error::Encode(Box::new(error)))?,
    )
    .execute(transaction.as_mut())
    .await
}

fn record_from_row(row: sqlx::sqlite::SqliteRow) -> Result<BackupRecord, BackupError> {
    let source = match row.get::<String, _>("source").as_str() {
        "manual" => BackupSource::Manual,
        "automatic" => BackupSource::Automatic,
        "upload" => BackupSource::Upload,
        "pre_restore" => BackupSource::PreRestore,
        _ => return Err(BackupError::Database),
    };
    let details_json: String = row.get("details_json");
    Ok(BackupRecord {
        id: row.get("id"),
        file_name: row.get("file_name"),
        source,
        status: row.get("status"),
        sha256: row.get("sha256"),
        size_bytes: row.get("size_bytes"),
        schema_version: row.get("schema_version"),
        created_at: row.get("created_at"),
        verified_at: row.get("verified_at"),
        created_by: row.get("created_by"),
        details: serde_json::from_str(&details_json).map_err(|_| BackupError::Database)?,
    })
}

async fn verify_header(path: &Path) -> Result<(), BackupError> {
    let mut file = fs::File::open(path)
        .await
        .map_err(|_| BackupError::Invalid)?;
    let mut header = [0_u8; 18];
    file.read_exact(&mut header)
        .await
        .map_err(|_| BackupError::Invalid)?;
    if &header[..16] != SQLITE_HEADER {
        return Err(BackupError::Invalid);
    }
    let encoded = u16::from_be_bytes([header[16], header[17]]);
    let page_size = if encoded == 1 {
        65_536
    } else {
        u32::from(encoded)
    };
    if !(512..=65_536).contains(&page_size) || !page_size.is_power_of_two() {
        return Err(BackupError::Invalid);
    }
    Ok(())
}

async fn hash_file(
    path: &Path,
    schema_version: i64,
    verified_at: time::OffsetDateTime,
) -> Result<VerifiedFile, BackupError> {
    let mut file = fs::File::open(path).await.map_err(|_| BackupError::Io)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut size = 0_i64;
    loop {
        let count = file.read(&mut buffer).await.map_err(|_| BackupError::Io)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        size = size
            .checked_add(i64::try_from(count).map_err(|_| BackupError::Invalid)?)
            .ok_or(BackupError::Invalid)?;
    }
    Ok(VerifiedFile {
        sha256: format!("{:x}", hasher.finalize()),
        size_bytes: size,
        schema_version,
        verified_at: canonical_timestamp(verified_at).map_err(|_| BackupError::Database)?,
    })
}

async fn install_database(
    source: &Path,
    live_path: &Path,
    operation_id: &str,
) -> Result<(), BackupError> {
    let parent = live_path.parent().ok_or(BackupError::Io)?;
    let staged = parent.join(format!(".restore-{operation_id}.sqlite3"));
    remove_if_exists(&staged).await;
    fs::copy(source, &staged)
        .await
        .map_err(|_| BackupError::Io)?;
    protect_file(&staged).await?;
    sync_file(&staged).await?;
    fs::rename(&staged, live_path)
        .await
        .map_err(|_| BackupError::Io)?;
    sync_directory(parent).await
}

fn manifest_path(database_path: &Path) -> PathBuf {
    database_path.with_extension("sqlite3.json")
}

async fn read_manifest(path: &Path) -> Result<BackupManifest, BackupError> {
    let bytes = fs::read(path).await.map_err(|_| BackupError::Invalid)?;
    if bytes.len() > 16 * 1024 {
        return Err(BackupError::Invalid);
    }
    let manifest: BackupManifest =
        serde_json::from_slice(&bytes).map_err(|_| BackupError::Invalid)?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn safe_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && Path::new(name).file_name() == Some(OsStr::new(name))
        && name.starts_with("remindi-")
        && name.ends_with(".sqlite3")
}

fn validate_manifest(manifest: &BackupManifest) -> Result<(), BackupError> {
    let expected_name = format!("remindi-{}.sqlite3", manifest.id);
    let details_size = serde_json::to_vec(&manifest.details)
        .map_err(|_| BackupError::Invalid)?
        .len();
    if Uuid::parse_str(&manifest.id).is_err()
        || manifest.file_name != expected_name
        || !safe_file_name(&manifest.file_name)
        || manifest.sha256.len() != 64
        || !manifest
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || manifest.size_bytes <= 0
        || !(1..=CURRENT_SCHEMA_VERSION).contains(&manifest.schema_version)
        || manifest.created_by.is_empty()
        || manifest.created_by.len() > 256
        || manifest.created_by.chars().any(char::is_control)
        || !manifest.details.is_object()
        || details_size > DETAIL_LIMIT
    {
        return Err(BackupError::Invalid);
    }
    Ok(())
}

fn is_temporary_name(name: &str) -> bool {
    name.starts_with(".tmp-") && (name.ends_with(".sqlite3") || name.ends_with(".manifest.json"))
}

async fn protect_directory(path: &Path) -> Result<(), BackupError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|_| BackupError::Io)?;
    }
    Ok(())
}

async fn protect_file(path: &Path) -> Result<(), BackupError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .await
            .map_err(|_| BackupError::Io)?;
    }
    Ok(())
}

async fn sync_file(path: &Path) -> Result<(), BackupError> {
    fs::File::open(path)
        .await
        .map_err(|_| BackupError::Io)?
        .sync_all()
        .await
        .map_err(|_| BackupError::Io)
}

async fn sync_directory(path: &Path) -> Result<(), BackupError> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || std::fs::File::open(path)?.sync_all())
        .await
        .map_err(|_| BackupError::Io)?
        .map_err(|_| BackupError::Io)
}

async fn remove_if_exists(path: &Path) {
    if let Err(error) = fs::remove_file(path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(event = "backup_cleanup_failed");
    }
}
