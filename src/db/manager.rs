use std::{path::Path, str::FromStr, sync::Arc, time::Duration};

use sqlx::{
    Sqlite, SqlitePool,
    pool::PoolConnection,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use thiserror::Error;
use tokio::sync::{OwnedRwLockReadGuard, RwLock};

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
    pool: SqlitePool,
    maintenance: Arc<RwLock<()>>,
}

pub struct DatabaseConnection {
    connection: PoolConnection<Sqlite>,
    _maintenance: OwnedRwLockReadGuard<()>,
}

impl AsMut<sqlx::SqliteConnection> for DatabaseConnection {
    fn as_mut(&mut self) -> &mut sqlx::SqliteConnection {
        self.connection.as_mut()
    }
}

impl DatabaseManager {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, DatabaseError> {
        let path = path.as_ref();
        validate_data_path(path)?;

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

        let manager = Self {
            pool,
            maintenance: Arc::new(RwLock::new(())),
        };
        manager.quick_check().await?;
        migrations::apply(&manager.pool).await?;
        manager.quick_check().await?;
        protect_database_file(path)?;
        Ok(manager)
    }

    pub async fn connection(&self) -> Result<DatabaseConnection, DatabaseError> {
        let maintenance = Arc::clone(&self.maintenance).read_owned().await;
        let connection = self.pool.acquire().await?;
        Ok(DatabaseConnection {
            connection,
            _maintenance: maintenance,
        })
    }

    pub async fn begin_immediate(&self) -> Result<ImmediateTransaction, DatabaseError> {
        ImmediateTransaction::begin(self).await
    }

    pub async fn close(self) -> Result<(), DatabaseError> {
        let _maintenance = self.maintenance.write().await;
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&self.pool)
            .await?;
        self.pool.close().await;
        Ok(())
    }

    async fn quick_check(&self) -> Result<(), DatabaseError> {
        let result: String = sqlx::query_scalar("PRAGMA quick_check")
            .fetch_one(&self.pool)
            .await?;
        if result == "ok" {
            Ok(())
        } else {
            Err(DatabaseError::Integrity)
        }
    }

    pub(super) fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub(super) fn maintenance(&self) -> &Arc<RwLock<()>> {
        &self.maintenance
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
