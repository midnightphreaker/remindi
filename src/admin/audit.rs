use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;

use crate::{
    clock::{Clock, IdGenerator},
    db::{DatabaseManager, ImmediateTransaction},
    remindi::canonical_timestamp,
};

use super::{AdminActor, AdminError};

/// One immutable administrative audit record.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdminEvent {
    pub sequence: i64,
    pub event_id: String,
    pub event_type: String,
    pub actor_id: String,
    pub request_id: Option<String>,
    pub occurred_at: String,
    pub outcome: String,
    pub details: Value,
}

pub(super) async fn append(
    database: &DatabaseManager,
    clock: &dyn Clock,
    ids: &dyn IdGenerator,
    event_type: &'static str,
    actor: &AdminActor,
    outcome: &'static str,
    details: &Value,
) -> Result<(), AdminError> {
    let mut transaction = database
        .begin_immediate()
        .await
        .map_err(|_| AdminError::Database)?;
    insert(
        &mut transaction,
        clock,
        ids,
        event_type,
        actor,
        outcome,
        details,
    )
    .await?;
    transaction.commit().await.map_err(|_| AdminError::Database)
}

pub(super) async fn insert(
    transaction: &mut ImmediateTransaction,
    clock: &dyn Clock,
    ids: &dyn IdGenerator,
    event_type: &'static str,
    actor: &AdminActor,
    outcome: &'static str,
    details: &Value,
) -> Result<(), AdminError> {
    let occurred_at = canonical_timestamp(clock.now()).map_err(|_| AdminError::Database)?;
    let details_json = serde_json::to_string(details).map_err(|_| AdminError::Database)?;
    sqlx::query(
        "INSERT INTO admin_events(\
             event_id, event_type, actor_id, request_id, occurred_at, outcome, details_json\
         ) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(ids.next_id().to_string())
    .bind(event_type)
    .bind(actor.actor_id())
    .bind(actor.request_id())
    .bind(occurred_at)
    .bind(outcome)
    .bind(details_json)
    .execute(transaction.as_mut())
    .await
    .map_err(|_| AdminError::Database)?;
    Ok(())
}

pub(super) async fn list(
    database: &DatabaseManager,
    after_sequence: Option<i64>,
    limit: u16,
) -> Result<Vec<AdminEvent>, AdminError> {
    if limit == 0 || limit > 200 || after_sequence.is_some_and(|value| value < 0) {
        return Err(AdminError::Validation);
    }
    let mut connection = database
        .connection()
        .await
        .map_err(|_| AdminError::Database)?;
    let rows = sqlx::query(
        "SELECT sequence, event_id, event_type, actor_id, request_id, \
                occurred_at, outcome, details_json \
         FROM admin_events WHERE sequence > ? ORDER BY sequence LIMIT ?",
    )
    .bind(after_sequence.unwrap_or(0))
    .bind(i64::from(limit))
    .fetch_all(connection.as_mut())
    .await
    .map_err(|_| AdminError::Database)?;

    rows.into_iter()
        .map(|row| {
            let details_json: String = row.get("details_json");
            Ok(AdminEvent {
                sequence: row.get("sequence"),
                event_id: row.get("event_id"),
                event_type: row.get("event_type"),
                actor_id: row.get("actor_id"),
                request_id: row.get("request_id"),
                occurred_at: row.get("occurred_at"),
                outcome: row.get("outcome"),
                details: serde_json::from_str(&details_json).map_err(|_| AdminError::Database)?,
            })
        })
        .collect()
}
