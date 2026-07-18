use std::{
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use sqlx::{
    Sqlite, SqlitePool,
    pool::PoolConnection,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use thiserror::Error;
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

use super::{migrations, transactions::ImmediateTransaction};

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("database path has no parent directory")]
    MissingParent,
    #[error("database data directory is world-writable")]
    WorldWritableDirectory,
    #[error("database path is not owned by the current process user")]
    WrongOwner,
    #[error("database uses newer schema version {0}")]
    NewerSchema(i64),
    #[error("database migration {0} has drifted")]
    MigrationDrift(i64),
    #[error("database bootstrap control rows are invalid")]
    InvalidBootstrapRows,
    #[error("database integrity check failed")]
    Integrity,
    #[error("database maintenance is active")]
    MaintenanceActive,
    #[error("database pool is closed for maintenance")]
    Closed,
    #[error("database operation failed")]
    Sql(#[source] sqlx::Error),
    #[error("database filesystem operation failed")]
    Io(#[source] std::io::Error),
}

impl From<sqlx::Error> for DatabaseError {
    fn from(error: sqlx::Error) -> Self {
        Self::Sql(error)
    }
}

impl From<std::io::Error> for DatabaseError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug)]
pub struct DatabaseManager {
    path: PathBuf,
    pool: RwLock<Option<SqlitePool>>,
    maintenance: Arc<RwLock<()>>,
}

pub struct DatabaseConnection {
    connection: PoolConnection<Sqlite>,
    _maintenance: OwnedRwLockReadGuard<()>,
}

/// Exclusive database boundary used only by guarded restore and shutdown.
pub struct DatabaseMaintenance<'a> {
    manager: &'a DatabaseManager,
    _maintenance: OwnedRwLockWriteGuard<()>,
}

impl AsMut<sqlx::SqliteConnection> for DatabaseConnection {
    fn as_mut(&mut self) -> &mut sqlx::SqliteConnection {
        self.connection.as_mut()
    }
}

impl DatabaseManager {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, DatabaseError> {
        let path = path.as_ref().to_path_buf();
        validate_data_path(&path)?;
        let pool = open_pool(&path).await?;
        let manager = Self {
            path: path.clone(),
            pool: RwLock::new(Some(pool)),
            maintenance: Arc::new(RwLock::new(())),
        };
        protect_database_file(&path)?;
        Ok(manager)
    }

    pub async fn connection(&self) -> Result<DatabaseConnection, DatabaseError> {
        let maintenance = Arc::clone(&self.maintenance)
            .try_read_owned()
            .map_err(|_| DatabaseError::MaintenanceActive)?;
        let pool = self.active_pool().await?;
        let connection = pool.acquire().await?;
        Ok(DatabaseConnection {
            connection,
            _maintenance: maintenance,
        })
    }

    pub async fn begin_immediate(&self) -> Result<ImmediateTransaction, DatabaseError> {
        ImmediateTransaction::begin(self).await
    }

    pub async fn close(self) -> Result<(), DatabaseError> {
        let mut maintenance = self.begin_maintenance().await;
        maintenance.checkpoint_and_close().await?;
        Ok(())
    }

    /// Acquires the exclusive maintenance boundary after in-flight requests drain.
    pub async fn begin_maintenance(&self) -> DatabaseMaintenance<'_> {
        let maintenance = Arc::clone(&self.maintenance).write_owned().await;
        DatabaseMaintenance {
            manager: self,
            _maintenance: maintenance,
        }
    }

    /// Reports whether maintenance is active or queued.
    #[must_use]
    pub fn maintenance_active(&self) -> bool {
        self.maintenance.try_read().is_err()
    }

    /// Returns the configured live database path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(super) async fn active_pool(&self) -> Result<SqlitePool, DatabaseError> {
        self.pool
            .read()
            .await
            .as_ref()
            .cloned()
            .ok_or(DatabaseError::Closed)
    }

    pub(super) fn maintenance(&self) -> &Arc<RwLock<()>> {
        &self.maintenance
    }
}

impl DatabaseMaintenance<'_> {
    /// Checkpoints WAL, closes every SQLx connection, and leaves the pool absent.
    pub async fn checkpoint_and_close(&mut self) -> Result<(), DatabaseError> {
        let pool = self
            .manager
            .pool
            .write()
            .await
            .take()
            .ok_or(DatabaseError::Closed)?;
        let checkpoint = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&pool)
            .await;
        pool.close().await;
        checkpoint?;
        Ok(())
    }

    /// Reopens the configured path, applies supported migrations, and validates it.
    pub async fn reopen(&mut self) -> Result<(), DatabaseError> {
        let pool = open_pool(&self.manager.path).await?;
        *self.manager.pool.write().await = Some(pool);
        Ok(())
    }

    /// Clears process-local scheduler leases in the active replacement database.
    pub async fn clear_scheduler_leases(&self) -> Result<(), DatabaseError> {
        let pool = self.manager.active_pool().await?;
        sqlx::query("DELETE FROM scheduler_leases")
            .execute(&pool)
            .await?;
        Ok(())
    }
}

async fn open_pool(path: &Path) -> Result<SqlitePool, DatabaseError> {
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Full)
        .foreign_keys(true)
        .busy_timeout(Duration::from_millis(5_000));
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::raw_sql(
                    "PRAGMA foreign_keys = ON;
                     PRAGMA synchronous = FULL;
                     PRAGMA busy_timeout = 5000;",
                )
                .execute(connection)
                .await?;
                Ok(())
            })
        })
        .connect_with(options)
        .await?;
    quick_check(&pool).await?;
    migrations::apply(&pool).await?;
    quick_check(&pool).await?;
    protect_database_file(path)?;
    Ok(pool)
}

async fn quick_check(pool: &SqlitePool) -> Result<(), DatabaseError> {
    let result: String = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_one(pool)
        .await?;
    if result == "ok" {
        Ok(())
    } else {
        Err(DatabaseError::Integrity)
    }
}

fn validate_data_path(path: &Path) -> Result<(), DatabaseError> {
    let parent = path.parent().ok_or(DatabaseError::MissingParent)?;
    std::fs::create_dir_all(parent)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let current_uid = current_uid();
        let metadata = std::fs::metadata(parent)?;
        if metadata.mode() & 0o002 != 0 {
            return Err(DatabaseError::WorldWritableDirectory);
        }
        if metadata.uid() != current_uid {
            return Err(DatabaseError::WrongOwner);
        }
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;

        if path.exists() {
            let metadata = std::fs::metadata(path)?;
            if metadata.mode() & 0o002 != 0 {
                return Err(DatabaseError::WorldWritableDirectory);
            }
            if metadata.uid() != current_uid {
                return Err(DatabaseError::WrongOwner);
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // Metadata ownership is validated against the current process without libc.
    std::fs::metadata("/proc/self")
        .map(|metadata| {
            use std::os::unix::fs::MetadataExt;
            metadata.uid()
        })
        .unwrap_or_else(|_| {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(".")
                .map(|metadata| metadata.uid())
                .unwrap_or(0)
        })
}

fn protect_database_file(path: &Path) -> Result<(), DatabaseError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
