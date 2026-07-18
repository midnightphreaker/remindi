use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{QueryBuilder, Row, Sqlite, SqliteConnection};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    EventType, EvidenceType, Remindi, RemindiEvent, RemindiLink, RemindiState, Trigger,
    canonical_timestamp, parse_timestamp,
};

#[derive(Debug, Error)]
pub(crate) enum RepositoryError {
    #[error("database operation failed")]
    Sql(#[from] sqlx::Error),
    #[error("stored Remindi value is invalid")]
    InvalidData,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CompletionEvidence {
    pub id: Uuid,
    pub remindi_id: Uuid,
    pub evidence_type: EvidenceType,
    pub summary: String,
    pub reference_uri: Option<String>,
    pub content_hash: Option<String>,
    pub observed_at: OffsetDateTime,
    pub recorded_at: OffsetDateTime,
    pub recorded_by: String,
    pub metadata: Option<Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HistoryPage {
    pub items: Vec<RemindiEvent>,
    pub evidence: Option<CompletionEvidence>,
    pub next_cursor: Option<String>,
}

pub(crate) struct RemindiRepository;

pub(crate) struct ListFilter<'a> {
    pub owner_id: &'a str,
    pub project_id: Option<&'a str>,
    pub task_id: Option<&'a str>,
    pub states: &'a [RemindiState],
    pub trigger_types: &'a [String],
    pub linked_goal_id: Option<&'a str>,
    pub linked_memory_hash: Option<&'a str>,
    pub after: Option<(&'a str, Uuid)>,
    pub limit: usize,
}

pub(crate) struct HistoryFilter<'a> {
    pub owner_id: &'a str,
    pub remindi_id: Uuid,
    pub after_sequence: i64,
    pub event_types: &'a [EventType],
    pub limit: usize,
}

pub(crate) struct IdempotencyRecord {
    pub request_hash: String,
    pub response_json: String,
}

pub(crate) struct NewIdempotency<'a> {
    pub actor_id: &'a str,
    pub tool_name: &'a str,
    pub key: &'a str,
    pub request_hash: &'a str,
    pub response_json: &'a str,
    pub remindi_id: Uuid,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
}

impl RemindiRepository {
    pub(crate) async fn find(
        connection: &mut SqliteConnection,
        owner_id: &str,
        id: Uuid,
    ) -> Result<Option<Remindi>, RepositoryError> {
        let row = sqlx::query("SELECT * FROM remindi WHERE owner_id = ? AND id = ?")
            .bind(owner_id)
            .bind(id.to_string())
            .fetch_optional(connection)
            .await?;
        row.as_ref().map(remindi_from_row).transpose()
    }

    pub(crate) async fn insert(
        connection: &mut SqliteConnection,
        item: &Remindi,
    ) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO remindi (
                id, owner_id, project_id, task_id, message, instructions, state, priority,
                trigger_type, trigger_spec_json, recurrence_spec_json, next_fire_at,
                next_evaluation_at, original_next_fire_at, due_since, snooze_until,
                snoozed_from_state, overdue_after_seconds, occurrence_no, source_session_id,
                source_task_lineage_id, last_checked_at, last_condition_status,
                last_condition_detail, snooze_count, version, created_at, updated_at,
                completed_at, cancelled_at
             ) VALUES (
                ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                ?, ?, ?, ?, ?
             )",
        )
        .bind(item.id.to_string())
        .bind(&item.owner_id)
        .bind(&item.project_id)
        .bind(&item.task_id)
        .bind(&item.message)
        .bind(&item.instructions)
        .bind(item.state.to_string())
        .bind(item.priority.to_string())
        .bind(trigger_name(&item.trigger))
        .bind(json_string(&item.trigger)?)
        .bind(optional_json_string(item.recurrence.as_ref())?)
        .bind(optional_timestamp(item.next_fire_at)?)
        .bind(optional_timestamp(item.next_evaluation_at)?)
        .bind(optional_timestamp(item.original_next_fire_at)?)
        .bind(optional_timestamp(item.due_since)?)
        .bind(optional_timestamp(item.snooze_until)?)
        .bind(item.snoozed_from_state.map(|state| state.to_string()))
        .bind(i64_from_u64(item.overdue_after_seconds)?)
        .bind(i64_from_u64(item.occurrence_no)?)
        .bind(&item.source_session_id)
        .bind(&item.source_task_lineage_id)
        .bind(optional_timestamp(item.last_checked_at)?)
        .bind(item.last_condition_status.map(|status| status.to_string()))
        .bind(&item.last_condition_detail)
        .bind(i64_from_u64(item.snooze_count)?)
        .bind(i64_from_u64(item.version)?)
        .bind(timestamp(item.created_at)?)
        .bind(timestamp(item.updated_at)?)
        .bind(optional_timestamp(item.completed_at)?)
        .bind(optional_timestamp(item.cancelled_at)?)
        .execute(connection)
        .await?;
        Ok(())
    }

    pub(crate) async fn update_cas(
        connection: &mut SqliteConnection,
        item: &Remindi,
        expected_version: u64,
    ) -> Result<bool, RepositoryError> {
        let result = sqlx::query(
            "UPDATE remindi SET
                message = ?, instructions = ?, state = ?, priority = ?, trigger_type = ?,
                trigger_spec_json = ?, recurrence_spec_json = ?, next_fire_at = ?,
                next_evaluation_at = ?, original_next_fire_at = ?, due_since = ?,
                snooze_until = ?, snoozed_from_state = ?, overdue_after_seconds = ?,
                occurrence_no = ?, source_session_id = ?, source_task_lineage_id = ?,
                last_checked_at = ?, last_condition_status = ?, last_condition_detail = ?,
                snooze_count = ?, version = ?, updated_at = ?, completed_at = ?, cancelled_at = ?
             WHERE owner_id = ? AND id = ? AND version = ?",
        )
        .bind(&item.message)
        .bind(&item.instructions)
        .bind(item.state.to_string())
        .bind(item.priority.to_string())
        .bind(trigger_name(&item.trigger))
        .bind(json_string(&item.trigger)?)
        .bind(optional_json_string(item.recurrence.as_ref())?)
        .bind(optional_timestamp(item.next_fire_at)?)
        .bind(optional_timestamp(item.next_evaluation_at)?)
        .bind(optional_timestamp(item.original_next_fire_at)?)
        .bind(optional_timestamp(item.due_since)?)
        .bind(optional_timestamp(item.snooze_until)?)
        .bind(item.snoozed_from_state.map(|state| state.to_string()))
        .bind(i64_from_u64(item.overdue_after_seconds)?)
        .bind(i64_from_u64(item.occurrence_no)?)
        .bind(&item.source_session_id)
        .bind(&item.source_task_lineage_id)
        .bind(optional_timestamp(item.last_checked_at)?)
        .bind(item.last_condition_status.map(|status| status.to_string()))
        .bind(&item.last_condition_detail)
        .bind(i64_from_u64(item.snooze_count)?)
        .bind(i64_from_u64(item.version)?)
        .bind(timestamp(item.updated_at)?)
        .bind(optional_timestamp(item.completed_at)?)
        .bind(optional_timestamp(item.cancelled_at)?)
        .bind(&item.owner_id)
        .bind(item.id.to_string())
        .bind(i64_from_u64(expected_version)?)
        .execute(connection)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn replace_links(
        connection: &mut SqliteConnection,
        item: &Remindi,
        links: &[RemindiLink],
    ) -> Result<(), RepositoryError> {
        sqlx::query("DELETE FROM remindi_links WHERE remindi_id = ?")
            .bind(item.id.to_string())
            .execute(&mut *connection)
            .await?;
        for link in links {
            sqlx::query(
                "INSERT INTO remindi_links(remindi_id, link_type, link_value, created_at)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(item.id.to_string())
            .bind(link.link_type.to_string())
            .bind(&link.value)
            .bind(timestamp(link.created_at)?)
            .execute(&mut *connection)
            .await?;
        }
        Ok(())
    }

    pub(crate) async fn append_event(
        connection: &mut SqliteConnection,
        event: &RemindiEvent,
    ) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO remindi_events (
                event_id, remindi_id, event_type, actor_type, actor_id, request_id,
                occurred_at, prior_version, new_version, details_json
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(event.event_id.to_string())
        .bind(event.remindi_id.to_string())
        .bind(event.event_type.to_string())
        .bind(event.actor_type.to_string())
        .bind(&event.actor_id)
        .bind(&event.request_id)
        .bind(timestamp(event.occurred_at)?)
        .bind(event.prior_version.map(i64_from_u64).transpose()?)
        .bind(event.new_version.map(i64_from_u64).transpose()?)
        .bind(json_string(&event.details)?)
        .execute(connection)
        .await?;
        Ok(())
    }

    pub(crate) async fn insert_evidence(
        connection: &mut SqliteConnection,
        evidence: &CompletionEvidence,
    ) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO completion_evidence (
                id, remindi_id, evidence_type, summary, reference_uri, content_hash,
                observed_at, recorded_at, recorded_by, metadata_json
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(evidence.id.to_string())
        .bind(evidence.remindi_id.to_string())
        .bind(evidence.evidence_type.to_string())
        .bind(&evidence.summary)
        .bind(&evidence.reference_uri)
        .bind(&evidence.content_hash)
        .bind(timestamp(evidence.observed_at)?)
        .bind(timestamp(evidence.recorded_at)?)
        .bind(&evidence.recorded_by)
        .bind(optional_json_string(evidence.metadata.as_ref())?)
        .execute(connection)
        .await?;
        Ok(())
    }

    pub(crate) async fn idempotency(
        connection: &mut SqliteConnection,
        actor_id: &str,
        tool_name: &str,
        key: &str,
    ) -> Result<Option<IdempotencyRecord>, RepositoryError> {
        let row = sqlx::query(
            "SELECT request_hash, response_json FROM idempotency_records
             WHERE actor_id = ? AND tool_name = ? AND idempotency_key = ?",
        )
        .bind(actor_id)
        .bind(tool_name)
        .bind(key)
        .fetch_optional(connection)
        .await?;
        Ok(row.map(|row| IdempotencyRecord {
            request_hash: row.get("request_hash"),
            response_json: row.get("response_json"),
        }))
    }

    pub(crate) async fn insert_idempotency(
        connection: &mut SqliteConnection,
        record: NewIdempotency<'_>,
    ) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO idempotency_records (
                actor_id, tool_name, idempotency_key, request_hash, response_json,
                remindi_id, created_at, expires_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(record.actor_id)
        .bind(record.tool_name)
        .bind(record.key)
        .bind(record.request_hash)
        .bind(record.response_json)
        .bind(record.remindi_id.to_string())
        .bind(timestamp(record.created_at)?)
        .bind(timestamp(record.expires_at)?)
        .execute(connection)
        .await?;
        Ok(())
    }

    pub(crate) async fn list(
        connection: &mut SqliteConnection,
        filter: ListFilter<'_>,
    ) -> Result<Vec<Remindi>, RepositoryError> {
        let mut query =
            QueryBuilder::<Sqlite>::new("SELECT r.* FROM remindi r WHERE r.owner_id = ");
        query.push_bind(filter.owner_id);
        if let Some(project_id) = filter.project_id {
            query.push(" AND r.project_id = ").push_bind(project_id);
        }
        if let Some(task_id) = filter.task_id {
            query.push(" AND r.task_id = ").push_bind(task_id);
        }
        push_enum_filter(&mut query, "r.state", filter.states);
        push_string_filter(&mut query, "r.trigger_type", filter.trigger_types);
        if let Some(goal) = filter.linked_goal_id {
            query
                .push(" AND EXISTS (SELECT 1 FROM remindi_links l WHERE l.remindi_id = r.id AND l.link_type = 'goal' AND l.link_value = ")
                .push_bind(goal)
                .push(")");
        }
        if let Some(memory) = filter.linked_memory_hash {
            query
                .push(" AND EXISTS (SELECT 1 FROM remindi_links l WHERE l.remindi_id = r.id AND l.link_type = 'memory' AND l.link_value = ")
                .push_bind(memory)
                .push(")");
        }
        if let Some((created_at, id)) = filter.after {
            query
                .push(" AND (r.created_at < ")
                .push_bind(created_at)
                .push(" OR (r.created_at = ")
                .push_bind(created_at)
                .push(" AND r.id > ")
                .push_bind(id.to_string())
                .push("))");
        }
        query
            .push(" ORDER BY r.created_at DESC, r.id ASC LIMIT ")
            .push_bind(i64::try_from(filter.limit).map_err(|_| RepositoryError::InvalidData)?);
        query
            .build()
            .fetch_all(connection)
            .await?
            .iter()
            .map(remindi_from_row)
            .collect()
    }

    pub(crate) async fn history(
        connection: &mut SqliteConnection,
        filter: HistoryFilter<'_>,
    ) -> Result<Vec<RemindiEvent>, RepositoryError> {
        if Self::find(connection, filter.owner_id, filter.remindi_id)
            .await?
            .is_none()
        {
            return Ok(vec![]);
        }
        let mut query =
            QueryBuilder::<Sqlite>::new("SELECT e.* FROM remindi_events e WHERE e.remindi_id = ");
        query
            .push_bind(filter.remindi_id.to_string())
            .push(" AND e.sequence > ")
            .push_bind(filter.after_sequence);
        push_enum_filter(&mut query, "e.event_type", filter.event_types);
        query
            .push(" ORDER BY e.sequence ASC LIMIT ")
            .push_bind(i64::try_from(filter.limit).map_err(|_| RepositoryError::InvalidData)?);
        query
            .build()
            .fetch_all(connection)
            .await?
            .iter()
            .map(event_from_row)
            .collect()
    }

    pub(crate) async fn evidence(
        connection: &mut SqliteConnection,
        owner_id: &str,
        remindi_id: Uuid,
    ) -> Result<Option<CompletionEvidence>, RepositoryError> {
        let row = sqlx::query(
            "SELECT e.* FROM completion_evidence e
             JOIN remindi r ON r.id = e.remindi_id
             WHERE r.owner_id = ? AND e.remindi_id = ?",
        )
        .bind(owner_id)
        .bind(remindi_id.to_string())
        .fetch_optional(connection)
        .await?;
        row.as_ref().map(evidence_from_row).transpose()
    }
}

fn push_enum_filter<T: ToString>(query: &mut QueryBuilder<Sqlite>, column: &str, values: &[T]) {
    if values.is_empty() {
        return;
    }
    query.push(" AND ").push(column).push(" IN (");
    let mut separated = query.separated(", ");
    for value in values {
        separated.push_bind(value.to_string());
    }
    separated.push_unseparated(")");
}

fn push_string_filter(query: &mut QueryBuilder<Sqlite>, column: &str, values: &[String]) {
    push_enum_filter(query, column, values);
}

fn remindi_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Remindi, RepositoryError> {
    Ok(Remindi {
        id: uuid(row, "id")?,
        owner_id: row.get("owner_id"),
        project_id: row.get("project_id"),
        task_id: row.get("task_id"),
        message: row.get("message"),
        instructions: row.get("instructions"),
        state: parse_enum(row, "state")?,
        priority: parse_enum(row, "priority")?,
        trigger: serde_json::from_str(row.get::<&str, _>("trigger_spec_json"))
            .map_err(|_| RepositoryError::InvalidData)?,
        recurrence: optional_json(row, "recurrence_spec_json")?,
        next_fire_at: optional_time(row, "next_fire_at")?,
        next_evaluation_at: optional_time(row, "next_evaluation_at")?,
        original_next_fire_at: optional_time(row, "original_next_fire_at")?,
        due_since: optional_time(row, "due_since")?,
        snooze_until: optional_time(row, "snooze_until")?,
        snoozed_from_state: optional_enum(row, "snoozed_from_state")?,
        overdue_after_seconds: u64_from_i64(row.get("overdue_after_seconds"))?,
        occurrence_no: u64_from_i64(row.get("occurrence_no"))?,
        source_session_id: row.get("source_session_id"),
        source_task_lineage_id: row.get("source_task_lineage_id"),
        last_checked_at: optional_time(row, "last_checked_at")?,
        last_condition_status: optional_enum(row, "last_condition_status")?,
        last_condition_detail: row.get("last_condition_detail"),
        snooze_count: u64_from_i64(row.get("snooze_count"))?,
        version: u64_from_i64(row.get("version"))?,
        created_at: required_time(row, "created_at")?,
        updated_at: required_time(row, "updated_at")?,
        completed_at: optional_time(row, "completed_at")?,
        cancelled_at: optional_time(row, "cancelled_at")?,
    })
}

fn event_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<RemindiEvent, RepositoryError> {
    Ok(RemindiEvent {
        sequence: Some(row.get("sequence")),
        event_id: uuid(row, "event_id")?,
        remindi_id: uuid(row, "remindi_id")?,
        event_type: parse_enum(row, "event_type")?,
        actor_type: parse_enum(row, "actor_type")?,
        actor_id: row.get("actor_id"),
        request_id: row.get("request_id"),
        occurred_at: required_time(row, "occurred_at")?,
        prior_version: optional_u64(row, "prior_version")?,
        new_version: optional_u64(row, "new_version")?,
        details: serde_json::from_str(row.get::<&str, _>("details_json"))
            .map_err(|_| RepositoryError::InvalidData)?,
    })
}

fn evidence_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<CompletionEvidence, RepositoryError> {
    Ok(CompletionEvidence {
        id: uuid(row, "id")?,
        remindi_id: uuid(row, "remindi_id")?,
        evidence_type: parse_enum(row, "evidence_type")?,
        summary: row.get("summary"),
        reference_uri: row.get("reference_uri"),
        content_hash: row.get("content_hash"),
        observed_at: required_time(row, "observed_at")?,
        recorded_at: required_time(row, "recorded_at")?,
        recorded_by: row.get("recorded_by"),
        metadata: optional_json(row, "metadata_json")?,
    })
}

fn trigger_name(trigger: &Trigger) -> &'static str {
    match trigger {
        Trigger::AtTime { .. } => "at_time",
        Trigger::AfterElapsed { .. } => "after_elapsed",
        Trigger::Interval { .. } => "interval",
        Trigger::NextSession => "next_session",
        Trigger::NextContinuation => "next_continuation",
        Trigger::GoalActive { .. } => "goal_active",
        Trigger::Condition { .. } => "condition",
    }
}

fn timestamp(value: OffsetDateTime) -> Result<String, RepositoryError> {
    canonical_timestamp(value).map_err(|_| RepositoryError::InvalidData)
}

fn optional_timestamp(value: Option<OffsetDateTime>) -> Result<Option<String>, RepositoryError> {
    value.map(timestamp).transpose()
}

fn required_time(
    row: &sqlx::sqlite::SqliteRow,
    column: &str,
) -> Result<OffsetDateTime, RepositoryError> {
    parse_timestamp(row.get(column)).map_err(|_| RepositoryError::InvalidData)
}

fn optional_time(
    row: &sqlx::sqlite::SqliteRow,
    column: &str,
) -> Result<Option<OffsetDateTime>, RepositoryError> {
    row.get::<Option<&str>, _>(column)
        .map(parse_timestamp)
        .transpose()
        .map_err(|_| RepositoryError::InvalidData)
}

fn uuid(row: &sqlx::sqlite::SqliteRow, column: &str) -> Result<Uuid, RepositoryError> {
    Uuid::parse_str(row.get(column)).map_err(|_| RepositoryError::InvalidData)
}

fn parse_enum<T: FromStr>(
    row: &sqlx::sqlite::SqliteRow,
    column: &str,
) -> Result<T, RepositoryError> {
    row.get::<&str, _>(column)
        .parse()
        .map_err(|_| RepositoryError::InvalidData)
}

fn optional_enum<T: FromStr>(
    row: &sqlx::sqlite::SqliteRow,
    column: &str,
) -> Result<Option<T>, RepositoryError> {
    row.get::<Option<&str>, _>(column)
        .map(str::parse)
        .transpose()
        .map_err(|_| RepositoryError::InvalidData)
}

fn optional_u64(
    row: &sqlx::sqlite::SqliteRow,
    column: &str,
) -> Result<Option<u64>, RepositoryError> {
    row.get::<Option<i64>, _>(column)
        .map(u64_from_i64)
        .transpose()
}

fn u64_from_i64(value: i64) -> Result<u64, RepositoryError> {
    u64::try_from(value).map_err(|_| RepositoryError::InvalidData)
}

fn i64_from_u64(value: u64) -> Result<i64, RepositoryError> {
    i64::try_from(value).map_err(|_| RepositoryError::InvalidData)
}

fn json_string<T: Serialize>(value: &T) -> Result<String, RepositoryError> {
    serde_json::to_string(value).map_err(|_| RepositoryError::InvalidData)
}

fn optional_json_string<T: Serialize>(
    value: Option<&T>,
) -> Result<Option<String>, RepositoryError> {
    value.map(json_string).transpose()
}

fn optional_json<T: for<'de> Deserialize<'de>>(
    row: &sqlx::sqlite::SqliteRow,
    column: &str,
) -> Result<Option<T>, RepositoryError> {
    row.get::<Option<&str>, _>(column)
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| RepositoryError::InvalidData)
}
