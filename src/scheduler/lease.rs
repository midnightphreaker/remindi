use std::sync::Arc;

use sqlx::Row;
use thiserror::Error;
use time::{Duration, OffsetDateTime};

use crate::{
    db::{DatabaseError, DatabaseManager},
    remindi::{canonical_timestamp, parse_timestamp},
};

const LEASE_NAME: &str = "trigger-evaluator";

/// A held scheduler lease and its optimistic version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseGuard {
    holder_id: String,
    version: u64,
    expires_at: OffsetDateTime,
    duration: Duration,
}

impl LeaseGuard {
    /// Returns the holder identity used by the compare-and-swap.
    #[must_use]
    pub fn holder_id(&self) -> &str {
        &self.holder_id
    }

    /// Returns the current lease version.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Returns whether renewal is due before half the duration remains.
    #[must_use]
    pub fn renewal_due(&self, now: OffsetDateTime) -> bool {
        now + self.duration / 2 >= self.expires_at
    }
}

/// Lease persistence or ownership failure.
#[derive(Debug, Error)]
pub enum LeaseError {
    #[error("the scheduler lease is held by another loop")]
    AlreadyHeld,
    #[error("the scheduler lease was lost")]
    Lost,
    #[error("the scheduler lease configuration is invalid")]
    InvalidConfiguration,
    #[error("scheduler lease database operation failed")]
    Database(#[from] DatabaseError),
    #[error("scheduler lease data is invalid")]
    InvalidData,
}

/// Atomic single-host lease operations over `scheduler_leases`.
pub struct SchedulerLease {
    database: Arc<DatabaseManager>,
    holder_id: String,
    duration: Duration,
}

impl SchedulerLease {
    /// Creates a lease client. `duration` must be positive.
    pub fn new(
        database: Arc<DatabaseManager>,
        holder_id: impl Into<String>,
        duration: Duration,
    ) -> Result<Self, LeaseError> {
        let holder_id = holder_id.into();
        if holder_id.trim().is_empty() || duration <= Duration::ZERO {
            return Err(LeaseError::InvalidConfiguration);
        }
        Ok(Self {
            database,
            holder_id,
            duration,
        })
    }

    /// Atomically acquires an absent or expired lease.
    pub async fn acquire(&self, now: OffsetDateTime) -> Result<LeaseGuard, LeaseError> {
        let mut transaction = self.database.begin_immediate().await?;
        let row = sqlx::query(
            "SELECT expires_at, version
             FROM scheduler_leases WHERE lease_name = ?",
        )
        .bind(LEASE_NAME)
        .fetch_optional(transaction.as_mut())
        .await
        .map_err(DatabaseError::from)?;
        let expires_at = now + self.duration;
        let expires = canonical_timestamp(expires_at).map_err(|_| LeaseError::InvalidData)?;
        let acquired = canonical_timestamp(now).map_err(|_| LeaseError::InvalidData)?;

        let version = if let Some(row) = row {
            let current_expiry =
                parse_timestamp(row.get("expires_at")).map_err(|_| LeaseError::InvalidData)?;
            let current_version =
                u64::try_from(row.get::<i64, _>("version")).map_err(|_| LeaseError::InvalidData)?;
            if current_expiry > now {
                transaction.rollback().await?;
                return Err(LeaseError::AlreadyHeld);
            }
            let next_version = current_version
                .checked_add(1)
                .ok_or(LeaseError::InvalidData)?;
            let result = sqlx::query(
                "UPDATE scheduler_leases
                 SET holder_id = ?, acquired_at = ?, expires_at = ?, version = ?
                 WHERE lease_name = ? AND version = ?",
            )
            .bind(&self.holder_id)
            .bind(acquired)
            .bind(expires)
            .bind(i64::try_from(next_version).map_err(|_| LeaseError::InvalidData)?)
            .bind(LEASE_NAME)
            .bind(i64::try_from(current_version).map_err(|_| LeaseError::InvalidData)?)
            .execute(transaction.as_mut())
            .await
            .map_err(DatabaseError::from)?;
            if result.rows_affected() != 1 {
                transaction.rollback().await?;
                return Err(LeaseError::Lost);
            }
            next_version
        } else {
            sqlx::query(
                "INSERT INTO scheduler_leases(
                    lease_name, holder_id, acquired_at, expires_at, version
                 ) VALUES (?, ?, ?, ?, 1)",
            )
            .bind(LEASE_NAME)
            .bind(&self.holder_id)
            .bind(acquired)
            .bind(expires)
            .execute(transaction.as_mut())
            .await
            .map_err(DatabaseError::from)?;
            1
        };
        transaction.commit().await?;
        Ok(LeaseGuard {
            holder_id: self.holder_id.clone(),
            version,
            expires_at,
            duration: self.duration,
        })
    }

    /// Renews a still-live lease using its holder and version.
    pub async fn renew(
        &self,
        guard: &mut LeaseGuard,
        now: OffsetDateTime,
    ) -> Result<(), LeaseError> {
        if guard.holder_id != self.holder_id || guard.expires_at <= now {
            return Err(LeaseError::Lost);
        }
        let next_version = guard
            .version
            .checked_add(1)
            .ok_or(LeaseError::InvalidData)?;
        let expires_at = now + self.duration;
        let result = sqlx::query(
            "UPDATE scheduler_leases
             SET expires_at = ?, version = ?
             WHERE lease_name = ? AND holder_id = ? AND version = ? AND expires_at > ?",
        )
        .bind(canonical_timestamp(expires_at).map_err(|_| LeaseError::InvalidData)?)
        .bind(i64::try_from(next_version).map_err(|_| LeaseError::InvalidData)?)
        .bind(LEASE_NAME)
        .bind(&self.holder_id)
        .bind(i64::try_from(guard.version).map_err(|_| LeaseError::InvalidData)?)
        .bind(canonical_timestamp(now).map_err(|_| LeaseError::InvalidData)?)
        .execute(self.database.connection().await?.as_mut())
        .await
        .map_err(DatabaseError::from)?;
        if result.rows_affected() != 1 {
            return Err(LeaseError::Lost);
        }
        guard.version = next_version;
        guard.expires_at = expires_at;
        Ok(())
    }

    /// Releases only the exact lease version owned by this loop.
    pub async fn release(&self, guard: &LeaseGuard) -> Result<(), LeaseError> {
        let result = sqlx::query(
            "DELETE FROM scheduler_leases
             WHERE lease_name = ? AND holder_id = ? AND version = ?",
        )
        .bind(LEASE_NAME)
        .bind(&self.holder_id)
        .bind(i64::try_from(guard.version).map_err(|_| LeaseError::InvalidData)?)
        .execute(self.database.connection().await?.as_mut())
        .await
        .map_err(DatabaseError::from)?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(LeaseError::Lost)
        }
    }
}
