use std::{sync::Arc, time::Duration as StdDuration};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::{Duration, OffsetDateTime, UtcOffset};
use uuid::Uuid;

use crate::{
    clock::{Clock, IdGenerator},
    db::DatabaseManager,
    triggers::{CheckContext, ConditionEvaluation, evaluate},
};

use super::{
    ActorType, CompletionEvidence, DomainError, EventType, EvidenceInput, HistoryPage,
    LifecycleEvent, LinkType, OccurrenceDisposition, Page, Priority, Readiness, RecurrenceSpec,
    Remindi, RemindiEvent, RemindiLink, RemindiState, Trigger, ValidatedEvidence,
    repository::{HistoryFilter, ListFilter, NewIdempotency, RemindiRepository, RepositoryError},
};

const IDEMPOTENCY_RETENTION: Duration = Duration::days(30);
const MAX_PAGE: usize = 200;

#[derive(Clone, Debug)]
pub struct Actor {
    pub actor_type: ActorType,
    pub actor_id: String,
    pub request_id: Option<String>,
}

impl Actor {
    #[must_use]
    pub fn agent(actor_id: impl Into<String>, request_id: Option<String>) -> Self {
        Self {
            actor_type: ActorType::Agent,
            actor_id: actor_id.into(),
            request_id,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LinkInput {
    pub link_type: LinkType,
    pub value: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AddRequest {
    pub project_id: String,
    pub task_id: Option<String>,
    pub message: String,
    pub instructions: Option<String>,
    pub priority: Priority,
    pub trigger: Trigger,
    pub recurrence: Option<RecurrenceSpec>,
    pub overdue_after_seconds: u64,
    pub links: Vec<LinkInput>,
    pub session_id: Option<String>,
    pub task_lineage_id: Option<String>,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CompleteRequest {
    pub remindi_id: Uuid,
    pub expected_version: u64,
    pub evidence: EvidenceInput,
    pub completion_note: Option<String>,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SnoozeRequest {
    pub remindi_id: Uuid,
    pub expected_version: u64,
    pub snooze_until: OffsetDateTime,
    pub reason: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UpdateRequest {
    pub remindi_id: Uuid,
    pub expected_version: u64,
    pub message: Option<String>,
    pub instructions: Option<Option<String>>,
    pub priority: Option<Priority>,
    pub trigger: Option<Trigger>,
    pub recurrence: Option<Option<RecurrenceSpec>>,
    pub overdue_after_seconds: Option<u64>,
    pub links: Option<Vec<LinkInput>>,
    pub occurrence_disposition: Option<OccurrenceDisposition>,
    pub reason: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CancelRequest {
    pub remindi_id: Uuid,
    pub expected_version: u64,
    pub reason: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Default)]
pub struct ListRequest {
    pub project_id: Option<String>,
    pub task_id: Option<String>,
    pub states: Vec<RemindiState>,
    pub trigger_types: Vec<String>,
    pub linked_goal_id: Option<String>,
    pub linked_memory_hash: Option<String>,
    pub limit: usize,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug)]
pub struct HistoryRequest {
    pub remindi_id: Uuid,
    pub after_sequence: Option<i64>,
    pub event_types: Vec<EventType>,
    pub limit: usize,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CheckRequest {
    pub project_id: String,
    pub task_id: Option<String>,
    pub session_id: Option<String>,
    pub task_lineage_id: Option<String>,
    pub lifecycle_event: LifecycleEvent,
    pub active_goal_ids: Vec<String>,
    pub include_scheduled: bool,
    pub limit: usize,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MutationResult {
    pub remindi: Remindi,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CheckedItem {
    pub remindi: Remindi,
    pub readiness: Readiness,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CheckResult {
    pub checked_at: OffsetDateTime,
    pub items: Vec<CheckedItem>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("input validation failed")]
    Validation,
    #[error("Remindi item was not found")]
    NotFound,
    #[error("operation is not allowed in the current state")]
    InvalidState,
    #[error("the Remindi item changed since it was read")]
    VersionConflict { current_version: u64 },
    #[error("idempotency key was reused with different input")]
    IdempotencyKeyReused,
    #[error("pagination cursor is invalid")]
    InvalidCursor,
    #[error("database is busy")]
    DatabaseBusy,
    #[error("internal service error")]
    Internal,
}

pub struct RemindiService {
    database: Arc<DatabaseManager>,
    owner_id: String,
    cursor_key: [u8; 32],
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

struct MutationCall<'a> {
    actor: &'a Actor,
    tool_name: &'a str,
    id: Uuid,
    expected_version: u64,
    idempotency_key: &'a str,
    request_hash: &'a str,
}

struct EventAppend<'a> {
    actor: &'a Actor,
    item: &'a Remindi,
    event_type: EventType,
    prior_version: Option<u64>,
    new_version: Option<u64>,
    details: Value,
}

impl RemindiService {
    #[must_use]
    pub fn new(
        database: Arc<DatabaseManager>,
        owner_id: impl Into<String>,
        mcp_secret: &[u8],
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"remindi:cursor-integrity:v1\0");
        hasher.update(mcp_secret);
        Self {
            database,
            owner_id: owner_id.into(),
            cursor_key: hasher.finalize().into(),
            clock,
            ids,
        }
    }

    pub async fn add(
        &self,
        actor: &Actor,
        mut request: AddRequest,
    ) -> Result<MutationResult, ServiceError> {
        validate_actor(actor)?;
        validate_idempotency_key(&request.idempotency_key)?;
        request.project_id = required_text(request.project_id, 512)?;
        request.message = required_text(request.message, 8192)?;
        request.task_id = optional_text(request.task_id, 512)?;
        request.instructions = optional_bounded(request.instructions, 32_768)?;
        request.session_id = optional_context(request.session_id, 512)?;
        request.task_lineage_id = optional_context(request.task_lineage_id, 512)?;
        normalize_trigger(&mut request.trigger)?;
        if let Some(recurrence) = request.recurrence.as_mut() {
            normalize_recurrence(recurrence)?;
        }
        request.trigger.validate().map_err(map_domain)?;
        if let Some(recurrence) = request.recurrence.as_ref() {
            recurrence
                .validate_for_trigger(&request.trigger)
                .map_err(map_domain)?;
        }
        if request.overdue_after_seconds > 31_536_000 {
            return Err(ServiceError::Validation);
        }
        validate_links(&request.links)?;
        validate_goal_link(&request.trigger, &request.links)?;

        let request_hash = request_hash(&request)?;
        let now = self.clock.now();
        let mut transaction = self
            .database
            .begin_immediate()
            .await
            .map_err(map_database)?;
        if let Some(replay) = replay(
            transaction.as_mut(),
            actor,
            "remindi_add",
            &request.idempotency_key,
            &request_hash,
        )
        .await?
        {
            return transaction
                .rollback()
                .await
                .map_err(map_database)
                .map(|()| replay);
        }

        let next_fire_at = initial_next_fire_at(&request.trigger, now)?;
        let item = Remindi {
            id: self.ids.next_id(),
            owner_id: self.owner_id.clone(),
            project_id: request.project_id.clone(),
            task_id: request.task_id.clone(),
            message: request.message.clone(),
            instructions: request.instructions.clone(),
            state: RemindiState::Scheduled,
            priority: request.priority,
            trigger: request.trigger.clone(),
            recurrence: request.recurrence.clone(),
            next_fire_at,
            next_evaluation_at: initial_next_evaluation_at(&request.trigger, now),
            original_next_fire_at: next_fire_at,
            due_since: None,
            snooze_until: None,
            snoozed_from_state: None,
            overdue_after_seconds: request.overdue_after_seconds,
            occurrence_no: 1,
            source_session_id: request.session_id.clone(),
            source_task_lineage_id: request.task_lineage_id.clone(),
            last_checked_at: None,
            last_condition_status: None,
            last_condition_detail: None,
            snooze_count: 0,
            version: 1,
            created_at: now,
            updated_at: now,
            completed_at: None,
            cancelled_at: None,
        };
        RemindiRepository::insert(transaction.as_mut(), &item)
            .await
            .map_err(map_repository)?;
        let links = links(&request.links, now);
        RemindiRepository::replace_links(transaction.as_mut(), &item, &links)
            .await
            .map_err(map_repository)?;
        self.append_event(
            transaction.as_mut(),
            EventAppend {
                actor,
                item: &item,
                event_type: EventType::Created,
                prior_version: None,
                new_version: Some(1),
                details: json!({
                    "trigger_type": trigger_type(&item.trigger),
                    "project_id": item.project_id,
                    "task_id": item.task_id,
                }),
            },
        )
        .await?;
        let response = MutationResult { remindi: item };
        store_idempotency(
            transaction.as_mut(),
            actor,
            "remindi_add",
            &request.idempotency_key,
            &request_hash,
            &response,
            now,
        )
        .await?;
        transaction.commit().await.map_err(map_database)?;
        Ok(response)
    }

    pub async fn complete(
        &self,
        actor: &Actor,
        mut request: CompleteRequest,
        maximum_future_skew: StdDuration,
    ) -> Result<MutationResult, ServiceError> {
        validate_actor(actor)?;
        validate_idempotency_key(&request.idempotency_key)?;
        validate_expected_version(request.expected_version)?;
        request.completion_note = optional_bounded(request.completion_note, 4096)?;
        let request_hash = request_hash(&request)?;
        let now = self.clock.now();
        let evidence = request
            .evidence
            .clone()
            .validate(now, maximum_future_skew)
            .map_err(map_domain)?;
        self.mutate(
            MutationCall {
                actor,
                tool_name: "remindi_complete",
                id: request.remindi_id,
                expected_version: request.expected_version,
                idempotency_key: &request.idempotency_key,
                request_hash: &request_hash,
            },
            move |item| {
                item.complete(&evidence, now).map_err(map_domain)?;
                Ok((
                    EventType::Completed,
                    json!({"completion_note": request.completion_note}),
                    Some(evidence),
                    None,
                ))
            },
        )
        .await
    }

    pub async fn snooze(
        &self,
        actor: &Actor,
        mut request: SnoozeRequest,
        maximum_horizon: StdDuration,
    ) -> Result<MutationResult, ServiceError> {
        validate_actor(actor)?;
        validate_idempotency_key(&request.idempotency_key)?;
        validate_expected_version(request.expected_version)?;
        request.reason = required_text(request.reason, 4096)?;
        request.snooze_until = normalize_instant(request.snooze_until)?;
        let request_hash = request_hash(&request)?;
        let now = self.clock.now();
        let maximum = Duration::try_from(maximum_horizon).map_err(|_| ServiceError::Validation)?;
        if request.snooze_until > now + maximum {
            return Err(ServiceError::Validation);
        }
        self.mutate(
            MutationCall {
                actor,
                tool_name: "remindi_snooze",
                id: request.remindi_id,
                expected_version: request.expected_version,
                idempotency_key: &request.idempotency_key,
                request_hash: &request_hash,
            },
            move |item| {
                let prior = item.next_fire_at;
                item.snooze(request.snooze_until, &request.reason, now)
                    .map_err(map_domain)?;
                Ok((
                    EventType::Snoozed,
                    json!({
                        "prior_next_fire_at": prior,
                        "snooze_until": request.snooze_until,
                        "reason": request.reason,
                    }),
                    None,
                    None,
                ))
            },
        )
        .await
    }

    pub async fn cancel(
        &self,
        actor: &Actor,
        mut request: CancelRequest,
    ) -> Result<MutationResult, ServiceError> {
        validate_actor(actor)?;
        validate_idempotency_key(&request.idempotency_key)?;
        validate_expected_version(request.expected_version)?;
        request.reason = required_text(request.reason, 4096)?;
        let request_hash = request_hash(&request)?;
        let now = self.clock.now();
        self.mutate(
            MutationCall {
                actor,
                tool_name: "remindi_cancel",
                id: request.remindi_id,
                expected_version: request.expected_version,
                idempotency_key: &request.idempotency_key,
                request_hash: &request_hash,
            },
            move |item| {
                item.cancel(&request.reason, now).map_err(map_domain)?;
                Ok((
                    EventType::Cancelled,
                    json!({"reason": request.reason}),
                    None,
                    None,
                ))
            },
        )
        .await
    }

    pub async fn update(
        &self,
        actor: &Actor,
        mut request: UpdateRequest,
    ) -> Result<MutationResult, ServiceError> {
        validate_actor(actor)?;
        validate_idempotency_key(&request.idempotency_key)?;
        validate_expected_version(request.expected_version)?;
        request.reason = required_text(request.reason, 4096)?;
        if !has_update(&request) {
            return Err(ServiceError::Validation);
        }
        if let Some(message) = request.message.take() {
            request.message = Some(required_text(message, 8192)?);
        }
        if let Some(instructions) = request.instructions.take() {
            request.instructions = Some(optional_bounded(instructions, 32_768)?);
        }
        if let Some(trigger) = request.trigger.as_ref() {
            trigger.validate().map_err(map_domain)?;
        }
        if let Some(trigger) = request.trigger.as_mut() {
            normalize_trigger(trigger)?;
        }
        if let Some(Some(recurrence)) = request.recurrence.as_mut() {
            normalize_recurrence(recurrence)?;
        }
        if request
            .overdue_after_seconds
            .is_some_and(|seconds| seconds > 31_536_000)
        {
            return Err(ServiceError::Validation);
        }
        if let Some(links) = request.links.as_ref() {
            validate_links(links)?;
        }
        if matches!(request.trigger.as_ref(), Some(Trigger::GoalActive { .. })) {
            validate_goal_link(
                request.trigger.as_ref().ok_or(ServiceError::Validation)?,
                request.links.as_deref().ok_or(ServiceError::Validation)?,
            )?;
        }
        let request_hash = request_hash(&request)?;
        let now = self.clock.now();
        self.mutate(
            MutationCall {
                actor,
                tool_name: "remindi_update",
                id: request.remindi_id,
                expected_version: request.expected_version,
                idempotency_key: &request.idempotency_key,
                request_hash: &request_hash,
            },
            move |item| {
                if item.state.is_terminal() {
                    return Err(ServiceError::InvalidState);
                }
                if request.occurrence_disposition.is_some()
                    && !matches!(item.state, RemindiState::Due | RemindiState::Overdue)
                {
                    return Err(ServiceError::InvalidState);
                }
                let prior_version = item.version;
                let prior_trigger = trigger_summary(item);
                let prior_occurrence_no = item.occurrence_no;
                let prior_schedule = item.next_fire_at;
                let mut changed_fields = Vec::new();
                let mut skipped_count = None;
                if let Some(message) = request.message {
                    item.message = message;
                    changed_fields.push("message");
                }
                if let Some(instructions) = request.instructions {
                    item.instructions = instructions;
                    changed_fields.push("instructions");
                }
                if let Some(priority) = request.priority {
                    item.priority = priority;
                    changed_fields.push("priority");
                }
                if let Some(seconds) = request.overdue_after_seconds {
                    item.overdue_after_seconds = seconds;
                    changed_fields.push("overdue_after_seconds");
                }
                if let Some(trigger) = request.trigger {
                    let next = initial_next_fire_at(&trigger, now)?;
                    let next_evaluation = initial_next_evaluation_at(&trigger, now);
                    item.replace_trigger(trigger, next, now)
                        .map_err(map_domain)?;
                    item.next_evaluation_at = next_evaluation;
                    changed_fields.push("trigger");
                }
                if let Some(recurrence) = request.recurrence {
                    item.recurrence = recurrence;
                    changed_fields.push("recurrence");
                }
                if let Some(spec) = item.recurrence.as_ref() {
                    spec.validate_for_trigger(&item.trigger)
                        .map_err(map_domain)?;
                }
                if let Some(disposition) = request.occurrence_disposition {
                    let recurrence = item.recurrence.as_ref().ok_or(ServiceError::InvalidState)?;
                    let anchor = item.next_fire_at.ok_or(ServiceError::InvalidState)?;
                    let advance = recurrence
                        .advance(anchor, item.occurrence_no, now, disposition)
                        .map_err(map_domain)?;
                    item.next_fire_at = Some(advance.next_fire_at);
                    item.original_next_fire_at = Some(advance.next_fire_at);
                    item.occurrence_no = advance.occurrence_no;
                    item.due_since = None;
                    item.state = RemindiState::Scheduled;
                    skipped_count = Some(advance.skipped_count);
                    changed_fields.push("occurrence_disposition");
                }
                if item.version == prior_version {
                    item.version += 1;
                    item.updated_at = now;
                }
                let replacement_links = request.links.map(|values| links(&values, now));
                if let Some(values) = replacement_links.as_ref() {
                    let inputs = values
                        .iter()
                        .map(|link| LinkInput {
                            link_type: link.link_type,
                            value: link.value.clone(),
                        })
                        .collect::<Vec<_>>();
                    validate_goal_link(&item.trigger, &inputs)?;
                    changed_fields.push("links");
                }
                let details = if request.occurrence_disposition.is_some() {
                    json!({
                        "reason": request.reason,
                        "previous_occurrence_no": prior_occurrence_no,
                        "next_occurrence_no": item.occurrence_no,
                        "previous_schedule": prior_schedule,
                        "next_schedule": item.next_fire_at,
                        "skipped_count": skipped_count.unwrap_or(0),
                    })
                } else {
                    json!({
                        "reason": request.reason,
                        "changed_fields": changed_fields,
                        "before_trigger": prior_trigger,
                        "after_trigger": trigger_summary(item),
                    })
                };
                Ok((
                    if request.occurrence_disposition.is_some() {
                        EventType::OccurrenceAdvanced
                    } else {
                        EventType::Updated
                    },
                    details,
                    None,
                    replacement_links,
                ))
            },
        )
        .await
    }

    pub async fn list(
        &self,
        actor: &Actor,
        request: ListRequest,
    ) -> Result<Page<Remindi>, ServiceError> {
        validate_actor(actor)?;
        validate_trigger_types(&request.trigger_types)?;
        let limit = page_limit(request.limit, 50)?;
        let cursor = request
            .cursor
            .as_deref()
            .map(|value| self.decode_cursor::<ListCursor>(value, "list"))
            .transpose()?;
        let cursor_time = cursor.as_ref().map(|cursor| cursor.created_at.as_str());
        let cursor_id = cursor
            .as_ref()
            .map(|cursor| Uuid::parse_str(&cursor.id).map_err(|_| ServiceError::InvalidCursor))
            .transpose()?;
        let mut connection = self.database.connection().await.map_err(map_database)?;
        let mut items = RemindiRepository::list(
            connection.as_mut(),
            ListFilter {
                owner_id: &self.owner_id,
                project_id: request.project_id.as_deref(),
                task_id: request.task_id.as_deref(),
                states: &request.states,
                trigger_types: &request.trigger_types,
                linked_goal_id: request.linked_goal_id.as_deref(),
                linked_memory_hash: request.linked_memory_hash.as_deref(),
                after: cursor_time.zip(cursor_id),
                limit: limit + 1,
            },
        )
        .await
        .map_err(map_repository)?;
        let has_more = items.len() > limit;
        items.truncate(limit);
        let next_cursor = if has_more {
            items
                .last()
                .map(|item| {
                    Ok(ListCursor {
                        created_at: super::canonical_timestamp(item.created_at)
                            .map_err(map_domain)?,
                        id: item.id.to_string(),
                    })
                })
                .transpose()?
                .map(|cursor| self.encode_cursor("list", &cursor))
                .transpose()?
        } else {
            None
        };
        Ok(Page { items, next_cursor })
    }

    pub async fn history(
        &self,
        actor: &Actor,
        request: HistoryRequest,
    ) -> Result<HistoryPage, ServiceError> {
        validate_actor(actor)?;
        let limit = page_limit(request.limit, 100)?;
        let cursor_after = request
            .cursor
            .as_deref()
            .map(|value| self.decode_cursor::<HistoryCursor>(value, "history"))
            .transpose()?
            .map(|cursor| cursor.sequence);
        let after = cursor_after.or(request.after_sequence).unwrap_or(0);
        if after < 0 {
            return Err(ServiceError::Validation);
        }
        let mut connection = self.database.connection().await.map_err(map_database)?;
        if RemindiRepository::find(connection.as_mut(), &self.owner_id, request.remindi_id)
            .await
            .map_err(map_repository)?
            .is_none()
        {
            return Err(ServiceError::NotFound);
        }
        let mut items = RemindiRepository::history(
            connection.as_mut(),
            HistoryFilter {
                owner_id: &self.owner_id,
                remindi_id: request.remindi_id,
                after_sequence: after,
                event_types: &request.event_types,
                limit: limit + 1,
            },
        )
        .await
        .map_err(map_repository)?;
        let has_more = items.len() > limit;
        items.truncate(limit);
        let next_cursor = if has_more {
            items
                .last()
                .and_then(|event| event.sequence)
                .map(|sequence| self.encode_cursor("history", &HistoryCursor { sequence }))
                .transpose()?
        } else {
            None
        };
        let evidence =
            RemindiRepository::evidence(connection.as_mut(), &self.owner_id, request.remindi_id)
                .await
                .map_err(map_repository)?;
        Ok(HistoryPage {
            items,
            evidence,
            next_cursor,
        })
    }

    pub async fn check(
        &self,
        actor: &Actor,
        mut request: CheckRequest,
    ) -> Result<CheckResult, ServiceError> {
        validate_actor(actor)?;
        request.project_id = required_text(request.project_id, 512)?;
        request.task_id = optional_text(request.task_id, 512)?;
        request.session_id = optional_context(request.session_id, 512)?;
        request.task_lineage_id = optional_context(request.task_lineage_id, 512)?;
        let limit = page_limit(request.limit, 50)?;
        let cursor = request
            .cursor
            .as_deref()
            .map(|value| self.decode_cursor::<CheckCursor>(value, "check"))
            .transpose()?;
        let mut connection = self.database.connection().await.map_err(map_database)?;
        let candidates = RemindiRepository::check_candidates(
            connection.as_mut(),
            &self.owner_id,
            &request.project_id,
            request.task_id.as_deref(),
        )
        .await
        .map_err(map_repository)?;
        drop(connection);
        let now = self.clock.now();
        let context = CheckContext {
            session_id: request.session_id,
            task_lineage_id: request.task_lineage_id,
            lifecycle_event: request.lifecycle_event,
            active_goal_ids: request.active_goal_ids,
        };
        let mut ready = Vec::new();
        for candidate in candidates {
            let mut preview = candidate.clone();
            let result = evaluate(
                &mut preview,
                now,
                &context,
                ConditionEvaluation::NotEvaluated,
            )
            .map_err(map_domain)?;
            if let Some(readiness) = result.readiness {
                ready.push((
                    candidate,
                    CheckedItem {
                        remindi: preview,
                        readiness,
                    },
                ));
            }
        }
        ready.sort_by(|left, right| check_order(&left.1, &right.1));
        if let Some(cursor) = cursor.as_ref() {
            ready.retain(|item| check_after_cursor(&item.1, cursor));
        }
        let has_more = ready.len() > limit;
        ready.truncate(limit);
        let next_cursor = if has_more {
            ready
                .last()
                .map(|item| check_cursor(&item.1))
                .transpose()?
                .map(|cursor| self.encode_cursor("check", &cursor))
                .transpose()?
        } else {
            None
        };
        let mut items = Vec::with_capacity(ready.len());
        for (candidate, _) in ready {
            if let Some(item) = self
                .evaluate_one(
                    actor,
                    candidate,
                    now,
                    &context,
                    ConditionEvaluation::NotEvaluated,
                    None,
                )
                .await?
            {
                items.push(item);
            }
        }
        Ok(CheckResult {
            checked_at: now,
            items,
            next_cursor,
        })
    }

    pub(crate) async fn scheduler_candidates(
        &self,
        now: OffsetDateTime,
        limit: usize,
    ) -> Result<Vec<Remindi>, ServiceError> {
        let mut connection = self.database.connection().await.map_err(map_database)?;
        RemindiRepository::scheduler_candidates(connection.as_mut(), &self.owner_id, now, limit)
            .await
            .map_err(map_repository)
    }

    pub(crate) async fn apply_scheduler_evaluation(
        &self,
        actor: &Actor,
        candidate: Remindi,
        now: OffsetDateTime,
        condition: ConditionEvaluation,
        condition_detail: Option<String>,
    ) -> Result<(), ServiceError> {
        let context = CheckContext {
            session_id: None,
            task_lineage_id: None,
            lifecycle_event: LifecycleEvent::Checkpoint,
            active_goal_ids: vec![],
        };
        self.evaluate_one(actor, candidate, now, &context, condition, condition_detail)
            .await
            .map(|_| ())
    }

    async fn evaluate_one(
        &self,
        actor: &Actor,
        mut candidate: Remindi,
        now: OffsetDateTime,
        context: &CheckContext,
        condition: ConditionEvaluation,
        condition_detail: Option<String>,
    ) -> Result<Option<CheckedItem>, ServiceError> {
        let prior = candidate.clone();
        let result = evaluate(&mut candidate, now, context, condition).map_err(map_domain)?;
        if condition != ConditionEvaluation::NotEvaluated {
            candidate.last_condition_detail = condition_detail.clone();
        }
        if result.events.is_empty() {
            return Ok(result.readiness.map(|readiness| CheckedItem {
                remindi: prior,
                readiness,
            }));
        }
        if candidate.version == prior.version {
            candidate.version += 1;
            candidate.updated_at = now;
        }
        let mut transaction = self
            .database
            .begin_immediate()
            .await
            .map_err(map_database)?;
        let current = RemindiRepository::find(transaction.as_mut(), &self.owner_id, candidate.id)
            .await
            .map_err(map_repository)?
            .ok_or(ServiceError::NotFound)?;
        if current.version != prior.version {
            transaction.rollback().await.map_err(map_database)?;
            return Ok(None);
        }
        if !RemindiRepository::update_cas(transaction.as_mut(), &candidate, prior.version)
            .await
            .map_err(map_repository)?
        {
            transaction.rollback().await.map_err(map_database)?;
            return Ok(None);
        }
        for event_type in result.events {
            self.append_event(
                transaction.as_mut(),
                EventAppend {
                    actor,
                    item: &candidate,
                    event_type,
                    prior_version: Some(prior.version),
                    new_version: Some(candidate.version),
                    details: json!({
                        "lifecycle_event": context.lifecycle_event,
                        "condition": candidate.last_condition_status,
                        "condition_detail": condition_detail,
                    }),
                },
            )
            .await?;
        }
        transaction.commit().await.map_err(map_database)?;
        Ok(result.readiness.map(|readiness| CheckedItem {
            remindi: candidate,
            readiness,
        }))
    }

    async fn mutate<F>(
        &self,
        call: MutationCall<'_>,
        operation: F,
    ) -> Result<MutationResult, ServiceError>
    where
        F: FnOnce(
            &mut Remindi,
        ) -> Result<
            (
                EventType,
                Value,
                Option<ValidatedEvidence>,
                Option<Vec<RemindiLink>>,
            ),
            ServiceError,
        >,
    {
        let now = self.clock.now();
        let mut transaction = self
            .database
            .begin_immediate()
            .await
            .map_err(map_database)?;
        if let Some(replay) = replay(
            transaction.as_mut(),
            call.actor,
            call.tool_name,
            call.idempotency_key,
            call.request_hash,
        )
        .await?
        {
            return transaction
                .rollback()
                .await
                .map_err(map_database)
                .map(|()| replay);
        }
        let mut item = RemindiRepository::find(transaction.as_mut(), &self.owner_id, call.id)
            .await
            .map_err(map_repository)?
            .ok_or(ServiceError::NotFound)?;
        if item.version != call.expected_version {
            return Err(ServiceError::VersionConflict {
                current_version: item.version,
            });
        }
        let prior_version = item.version;
        let (event_type, mut details, evidence, replacement_links) = operation(&mut item)?;
        if !RemindiRepository::update_cas(transaction.as_mut(), &item, prior_version)
            .await
            .map_err(map_repository)?
        {
            let current = RemindiRepository::find(transaction.as_mut(), &self.owner_id, call.id)
                .await
                .map_err(map_repository)?
                .ok_or(ServiceError::NotFound)?;
            return Err(ServiceError::VersionConflict {
                current_version: current.version,
            });
        }
        if let Some(links) = replacement_links {
            RemindiRepository::replace_links(transaction.as_mut(), &item, &links)
                .await
                .map_err(map_repository)?;
        }
        if let Some(evidence) = evidence {
            let evidence_record = CompletionEvidence {
                id: self.ids.next_id(),
                remindi_id: item.id,
                evidence_type: evidence.evidence_type(),
                summary: evidence.summary().to_owned(),
                reference_uri: evidence.reference_uri().map(str::to_owned),
                content_hash: evidence.content_hash().map(str::to_owned),
                observed_at: evidence.observed_at(),
                recorded_at: now,
                recorded_by: call.actor.actor_id.clone(),
                metadata: evidence.metadata().cloned(),
            };
            details
                .as_object_mut()
                .ok_or(ServiceError::Internal)?
                .insert(
                    "evidence_id".to_owned(),
                    Value::String(evidence_record.id.to_string()),
                );
            RemindiRepository::insert_evidence(transaction.as_mut(), &evidence_record)
                .await
                .map_err(map_repository)?;
        }
        self.append_event(
            transaction.as_mut(),
            EventAppend {
                actor: call.actor,
                item: &item,
                event_type,
                prior_version: Some(prior_version),
                new_version: Some(item.version),
                details,
            },
        )
        .await?;
        let response = MutationResult { remindi: item };
        store_idempotency(
            transaction.as_mut(),
            call.actor,
            call.tool_name,
            call.idempotency_key,
            call.request_hash,
            &response,
            now,
        )
        .await?;
        transaction.commit().await.map_err(map_database)?;
        Ok(response)
    }

    async fn append_event(
        &self,
        connection: &mut sqlx::SqliteConnection,
        event: EventAppend<'_>,
    ) -> Result<(), ServiceError> {
        RemindiRepository::append_event(
            connection,
            &RemindiEvent {
                sequence: None,
                event_id: self.ids.next_id(),
                remindi_id: event.item.id,
                event_type: event.event_type,
                actor_type: event.actor.actor_type,
                actor_id: event.actor.actor_id.clone(),
                request_id: event.actor.request_id.clone(),
                occurred_at: self.clock.now(),
                prior_version: event.prior_version,
                new_version: event.new_version,
                details: event.details,
            },
        )
        .await
        .map_err(map_repository)
    }

    fn encode_cursor<T: Serialize>(&self, kind: &str, value: &T) -> Result<String, ServiceError> {
        let payload = serde_json::to_vec(&CursorPayload {
            version: 1,
            kind,
            value,
        })
        .map_err(|_| ServiceError::Internal)?;
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.cursor_key).map_err(|_| ServiceError::Internal)?;
        mac.update(&payload);
        let envelope = CursorEnvelope {
            payload: URL_SAFE_NO_PAD.encode(payload),
            tag: URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()),
        };
        serde_json::to_vec(&envelope)
            .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
            .map_err(|_| ServiceError::Internal)
    }

    fn decode_cursor<T: for<'de> Deserialize<'de>>(
        &self,
        encoded: &str,
        kind: &str,
    ) -> Result<T, ServiceError> {
        let envelope_bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| ServiceError::InvalidCursor)?;
        let envelope: CursorEnvelope =
            serde_json::from_slice(&envelope_bytes).map_err(|_| ServiceError::InvalidCursor)?;
        let payload = URL_SAFE_NO_PAD
            .decode(envelope.payload)
            .map_err(|_| ServiceError::InvalidCursor)?;
        let tag = URL_SAFE_NO_PAD
            .decode(envelope.tag)
            .map_err(|_| ServiceError::InvalidCursor)?;
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.cursor_key).map_err(|_| ServiceError::Internal)?;
        mac.update(&payload);
        mac.verify_slice(&tag)
            .map_err(|_| ServiceError::InvalidCursor)?;
        let decoded: CursorPayloadOwned<T> =
            serde_json::from_slice(&payload).map_err(|_| ServiceError::InvalidCursor)?;
        if decoded.version != 1 || decoded.kind != kind {
            return Err(ServiceError::InvalidCursor);
        }
        Ok(decoded.value)
    }
}

#[derive(Serialize)]
struct CursorPayload<'a, T> {
    version: u8,
    kind: &'a str,
    value: &'a T,
}

#[derive(Deserialize)]
struct CursorPayloadOwned<T> {
    version: u8,
    kind: String,
    value: T,
}

#[derive(Deserialize, Serialize)]
struct CursorEnvelope {
    payload: String,
    tag: String,
}

#[derive(Deserialize, Serialize)]
struct ListCursor {
    created_at: String,
    id: String,
}

#[derive(Deserialize, Serialize)]
struct HistoryCursor {
    sequence: i64,
}

#[derive(Deserialize, Serialize)]
struct CheckCursor {
    readiness: u8,
    priority: u8,
    next_fire_at: Option<String>,
    id: String,
}

async fn replay(
    connection: &mut sqlx::SqliteConnection,
    actor: &Actor,
    tool_name: &str,
    key: &str,
    request_hash: &str,
) -> Result<Option<MutationResult>, ServiceError> {
    let record = RemindiRepository::idempotency(connection, &actor.actor_id, tool_name, key)
        .await
        .map_err(map_repository)?;
    let Some(record) = record else {
        return Ok(None);
    };
    if record.request_hash != request_hash {
        return Err(ServiceError::IdempotencyKeyReused);
    }
    serde_json::from_str(&record.response_json)
        .map(Some)
        .map_err(|_| ServiceError::Internal)
}

async fn store_idempotency(
    connection: &mut sqlx::SqliteConnection,
    actor: &Actor,
    tool_name: &str,
    key: &str,
    request_hash: &str,
    response: &MutationResult,
    now: OffsetDateTime,
) -> Result<(), ServiceError> {
    let response_json = serde_json::to_string(response).map_err(|_| ServiceError::Internal)?;
    RemindiRepository::insert_idempotency(
        connection,
        NewIdempotency {
            actor_id: &actor.actor_id,
            tool_name,
            key,
            request_hash,
            response_json: &response_json,
            remindi_id: response.remindi.id,
            created_at: now,
            expires_at: now + IDEMPOTENCY_RETENTION,
        },
    )
    .await
    .map_err(map_repository)
}

fn request_hash<T: Serialize>(request: &T) -> Result<String, ServiceError> {
    let bytes = serde_json::to_vec(request).map_err(|_| ServiceError::Internal)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn initial_next_fire_at(
    trigger: &Trigger,
    now: OffsetDateTime,
) -> Result<Option<OffsetDateTime>, ServiceError> {
    match trigger {
        Trigger::AtTime { at } => Ok(Some(*at)),
        Trigger::AfterElapsed { after_seconds } => {
            let seconds = i64::try_from(*after_seconds).map_err(|_| ServiceError::Validation)?;
            Ok(Some(now + Duration::seconds(seconds)))
        }
        Trigger::Interval { first_at, .. } => Ok(Some(*first_at)),
        Trigger::NextSession
        | Trigger::NextContinuation
        | Trigger::GoalActive { .. }
        | Trigger::Condition { .. } => Ok(None),
    }
}

fn initial_next_evaluation_at(trigger: &Trigger, now: OffsetDateTime) -> Option<OffsetDateTime> {
    match trigger {
        Trigger::Condition {
            poll_interval_seconds,
            manual_check_at,
            ..
        } => {
            let poll = i64::try_from(poll_interval_seconds.unwrap_or(30))
                .ok()
                .map(|seconds| now + Duration::seconds(seconds));
            match (poll, *manual_check_at) {
                (Some(poll), Some(manual)) => Some(poll.min(manual)),
                (poll, manual) => poll.or(manual),
            }
        }
        _ => None,
    }
}

fn normalize_instant(value: OffsetDateTime) -> Result<OffsetDateTime, ServiceError> {
    value
        .to_offset(UtcOffset::UTC)
        .replace_nanosecond((value.nanosecond() / 1_000_000) * 1_000_000)
        .map_err(|_| ServiceError::Validation)
}

fn normalize_trigger(trigger: &mut Trigger) -> Result<(), ServiceError> {
    match trigger {
        Trigger::AtTime { at } => *at = normalize_instant(*at)?,
        Trigger::Interval { first_at, .. } => *first_at = normalize_instant(*first_at)?,
        Trigger::Condition {
            manual_check_at, ..
        } => {
            if let Some(value) = manual_check_at.as_mut() {
                *value = normalize_instant(*value)?;
            }
        }
        Trigger::AfterElapsed { .. }
        | Trigger::NextSession
        | Trigger::NextContinuation
        | Trigger::GoalActive { .. } => {}
    }
    Ok(())
}

fn normalize_recurrence(recurrence: &mut RecurrenceSpec) -> Result<(), ServiceError> {
    if let Some(value) = recurrence.end_at.as_mut() {
        *value = normalize_instant(*value)?;
    }
    Ok(())
}

fn trigger_type(trigger: &Trigger) -> &'static str {
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

fn trigger_summary(item: &Remindi) -> Value {
    json!({
        "type": trigger_type(&item.trigger),
        "next_fire_at": item.next_fire_at,
        "next_evaluation_at": item.next_evaluation_at,
    })
}

fn validate_actor(actor: &Actor) -> Result<(), ServiceError> {
    if actor.actor_id.trim().is_empty() {
        Err(ServiceError::Validation)
    } else {
        Ok(())
    }
}

fn validate_idempotency_key(key: &str) -> Result<(), ServiceError> {
    if (8..=128).contains(&key.len())
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        Ok(())
    } else {
        Err(ServiceError::Validation)
    }
}

fn validate_expected_version(version: u64) -> Result<(), ServiceError> {
    if version == 0 {
        Err(ServiceError::Validation)
    } else {
        Ok(())
    }
}

fn required_text(value: String, maximum: usize) -> Result<String, ServiceError> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.chars().count() > maximum {
        Err(ServiceError::Validation)
    } else {
        Ok(trimmed.to_owned())
    }
}

fn optional_text(value: Option<String>, maximum: usize) -> Result<Option<String>, ServiceError> {
    value.map(|value| required_text(value, maximum)).transpose()
}

fn optional_context(value: Option<String>, maximum: usize) -> Result<Option<String>, ServiceError> {
    value
        .map(|value| {
            let trimmed = value.trim();
            if trimmed.chars().count() > maximum {
                Err(ServiceError::Validation)
            } else if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_owned()))
            }
        })
        .transpose()
        .map(Option::flatten)
}

fn validate_trigger_types(values: &[String]) -> Result<(), ServiceError> {
    if values.iter().all(|value| {
        matches!(
            value.as_str(),
            "at_time"
                | "after_elapsed"
                | "interval"
                | "next_session"
                | "next_continuation"
                | "goal_active"
                | "condition"
        )
    }) {
        Ok(())
    } else {
        Err(ServiceError::Validation)
    }
}

fn optional_bounded(value: Option<String>, maximum: usize) -> Result<Option<String>, ServiceError> {
    value
        .map(|value| {
            if value.chars().count() <= maximum {
                Ok(value)
            } else {
                Err(ServiceError::Validation)
            }
        })
        .transpose()
}

fn validate_links(links: &[LinkInput]) -> Result<(), ServiceError> {
    if links.len() > 100 {
        return Err(ServiceError::Validation);
    }
    for (index, link) in links.iter().enumerate() {
        if link.value.trim().is_empty()
            || link.value.chars().count() > 2048
            || links[..index]
                .iter()
                .any(|prior| prior.link_type == link.link_type && prior.value == link.value)
        {
            return Err(ServiceError::Validation);
        }
    }
    Ok(())
}

fn validate_goal_link(trigger: &Trigger, links: &[LinkInput]) -> Result<(), ServiceError> {
    let Trigger::GoalActive { goal_id } = trigger else {
        return Ok(());
    };
    let matching = links
        .iter()
        .filter(|link| link.link_type == LinkType::Goal && link.value == *goal_id)
        .count();
    let all_goals = links
        .iter()
        .filter(|link| link.link_type == LinkType::Goal)
        .count();
    if matching == 1 && all_goals == 1 {
        Ok(())
    } else {
        Err(ServiceError::Validation)
    }
}

fn links(values: &[LinkInput], now: OffsetDateTime) -> Vec<RemindiLink> {
    values
        .iter()
        .map(|link| RemindiLink {
            link_type: link.link_type,
            value: link.value.clone(),
            created_at: now,
        })
        .collect()
}

fn has_update(request: &UpdateRequest) -> bool {
    request.message.is_some()
        || request.instructions.is_some()
        || request.priority.is_some()
        || request.trigger.is_some()
        || request.recurrence.is_some()
        || request.overdue_after_seconds.is_some()
        || request.links.is_some()
        || request.occurrence_disposition.is_some()
}

fn page_limit(value: usize, default: usize) -> Result<usize, ServiceError> {
    let value = if value == 0 { default } else { value };
    if value <= MAX_PAGE {
        Ok(value)
    } else {
        Err(ServiceError::Validation)
    }
}

fn map_domain(error: DomainError) -> ServiceError {
    match error {
        DomainError::TerminalState
        | DomainError::SnoozeRequiresReadyState
        | DomainError::FinalOccurrence => ServiceError::InvalidState,
        _ => ServiceError::Validation,
    }
}

fn map_database(error: crate::db::DatabaseError) -> ServiceError {
    match error {
        crate::db::DatabaseError::Sql(sqlx::Error::Database(error))
            if sqlite_busy(error.as_ref()) =>
        {
            ServiceError::DatabaseBusy
        }
        _ => ServiceError::Internal,
    }
}

fn map_repository(error: RepositoryError) -> ServiceError {
    match error {
        RepositoryError::Sql(sqlx::Error::Database(error)) if sqlite_busy(error.as_ref()) => {
            ServiceError::DatabaseBusy
        }
        _ => ServiceError::Internal,
    }
}

fn sqlite_busy(error: &dyn sqlx::error::DatabaseError) -> bool {
    matches!(error.code().as_deref(), Some("5" | "6"))
}

fn check_order(left: &CheckedItem, right: &CheckedItem) -> std::cmp::Ordering {
    readiness_rank(left.readiness)
        .cmp(&readiness_rank(right.readiness))
        .then_with(|| {
            priority_rank(right.remindi.priority).cmp(&priority_rank(left.remindi.priority))
        })
        .then_with(
            || match (left.remindi.next_fire_at, right.remindi.next_fire_at) {
                (Some(left), Some(right)) => left.cmp(&right),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            },
        )
        .then_with(|| left.remindi.id.cmp(&right.remindi.id))
}

fn check_cursor(item: &CheckedItem) -> Result<CheckCursor, ServiceError> {
    Ok(CheckCursor {
        readiness: readiness_rank(item.readiness),
        priority: priority_rank(item.remindi.priority),
        next_fire_at: item
            .remindi
            .next_fire_at
            .map(super::canonical_timestamp)
            .transpose()
            .map_err(map_domain)?,
        id: item.remindi.id.to_string(),
    })
}

fn check_after_cursor(item: &CheckedItem, cursor: &CheckCursor) -> bool {
    let next_fire_at = item
        .remindi
        .next_fire_at
        .and_then(|value| super::canonical_timestamp(value).ok());
    (
        readiness_rank(item.readiness),
        std::cmp::Reverse(priority_rank(item.remindi.priority)),
        next_fire_at.is_none(),
        next_fire_at.unwrap_or_default(),
        item.remindi.id.to_string(),
    ) > (
        cursor.readiness,
        std::cmp::Reverse(cursor.priority),
        cursor.next_fire_at.is_none(),
        cursor.next_fire_at.clone().unwrap_or_default(),
        cursor.id.clone(),
    )
}

const fn readiness_rank(readiness: Readiness) -> u8 {
    match readiness {
        Readiness::Overdue => 0,
        Readiness::Due => 1,
        Readiness::ManualVerification => 2,
    }
}

const fn priority_rank(priority: Priority) -> u8 {
    match priority {
        Priority::Low => 0,
        Priority::Normal => 1,
        Priority::High => 2,
        Priority::Critical => 3,
    }
}
