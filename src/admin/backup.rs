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

use super::{AdminActor, audit};

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
}

pub struct BackupManager {
    database: Arc<DatabaseManager>,
    directory: PathBuf,
    owner_id: String,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    mutation: Mutex<()>,
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
        protect_directory(&self.directory).await
    }

    async fn verify_database(&self, path: &Path) -> Result<VerifiedFile, BackupError> {
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
        let wrong_owner: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM remindi WHERE owner_id <> ?")
                .bind(&self.owner_id)
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
        hash_file(path, schema_version, self.clock.now()).await
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
