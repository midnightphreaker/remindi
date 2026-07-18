use std::sync::Arc;

use sqlx::{Sqlite, Transaction};
use tokio::sync::OwnedRwLockReadGuard;

use super::{DatabaseError, DatabaseManager};

pub struct ImmediateTransaction {
    transaction: Transaction<'static, Sqlite>,
    _maintenance: OwnedRwLockReadGuard<()>,
}

impl ImmediateTransaction {
    pub(super) async fn begin(manager: &DatabaseManager) -> Result<Self, DatabaseError> {
        let maintenance = Arc::clone(manager.maintenance())
            .try_read_owned()
            .map_err(|_| DatabaseError::MaintenanceActive)?;
        let pool = manager.active_pool().await?;
        let transaction = pool.begin_with("BEGIN IMMEDIATE").await?;
        Ok(Self {
            transaction,
            _maintenance: maintenance,
        })
    }

    pub async fn commit(self) -> Result<(), DatabaseError> {
        self.transaction.commit().await?;
        Ok(())
    }

    pub async fn rollback(self) -> Result<(), DatabaseError> {
        self.transaction.rollback().await?;
        Ok(())
    }
}

impl AsMut<sqlx::SqliteConnection> for ImmediateTransaction {
    fn as_mut(&mut self) -> &mut sqlx::SqliteConnection {
        self.transaction.as_mut()
    }
}
