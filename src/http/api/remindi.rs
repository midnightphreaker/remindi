use std::time::Duration;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, Method},
    response::Response,
    routing::{get, post},
};
use serde::{Deserialize, Deserializer};
use serde_json::json;
use uuid::Uuid;

use crate::remindi::{
    self as domain, AddRequest, CancelRequest, CheckRequest, CompleteRequest, EvidenceInput,
    EvidenceSource, HistoryRequest, LinkInput, ListRequest, RemindiState, SnoozeRequest,
    UpdateRequest,
};

use super::{WebApiState, actor, authorize_mutation, service_error, success};

pub fn router() -> Router<WebApiState> {
    Router::new()
        .route("/remindi", get(list).post(add))
        .route("/remindi/check", post(check))
        .route("/remindi/{id}", get(detail).patch(update))
        .route("/remindi/{id}/complete", post(complete))
        .route("/remindi/{id}/snooze", post(snooze))
        .route("/remindi/{id}/cancel", post(cancel))
        .route("/remindi/{id}/history", get(history))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddBody {
    project_id: String,
    task_id: Option<String>,
    message: String,
    instructions: Option<String>,
    #[serde(default)]
    priority: Priority,
    trigger: WebTrigger,
    recurrence: Option<WebRecurrence>,
    #[serde(default)]
    overdue_after_seconds: u64,
    #[serde(default)]
    links: Vec<WebLink>,
    session_id: Option<String>,
    task_lineage_id: Option<String>,
    idempotency_key: String,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Priority {
    Low,
    #[default]
    Normal,
    High,
    Critical,
}

impl From<Priority> for domain::Priority {
    fn from(value: Priority) -> Self {
        match value {
            Priority::Low => Self::Low,
            Priority::Normal => Self::Normal,
            Priority::High => Self::High,
            Priority::Critical => Self::Critical,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum WebTrigger {
    AtTime {
        at: String,
    },
    AfterElapsed {
        after_seconds: u64,
    },
    Interval {
        first_at: String,
        every_seconds: u64,
    },
    NextSession,
    NextContinuation,
    GoalActive {
        goal_id: String,
    },
    Condition {
        adapter: String,
        parameters: serde_json::Value,
        poll_interval_seconds: Option<u64>,
        manual_check_at: Option<String>,
    },
}

impl WebTrigger {
    fn into_domain(self) -> Result<domain::Trigger, ()> {
        Ok(match self {
            Self::AtTime { at } => domain::Trigger::AtTime {
                at: domain::parse_timestamp(&at).map_err(|_| ())?,
            },
            Self::AfterElapsed { after_seconds } => domain::Trigger::AfterElapsed { after_seconds },
            Self::Interval {
                first_at,
                every_seconds,
            } => domain::Trigger::Interval {
                first_at: domain::parse_timestamp(&first_at).map_err(|_| ())?,
                every_seconds,
            },
            Self::NextSession => domain::Trigger::NextSession,
            Self::NextContinuation => domain::Trigger::NextContinuation,
            Self::GoalActive { goal_id } => domain::Trigger::GoalActive { goal_id },
            Self::Condition {
                adapter,
                parameters,
                poll_interval_seconds,
                manual_check_at,
            } => domain::Trigger::Condition {
                adapter,
                parameters,
                poll_interval_seconds,
                manual_check_at: manual_check_at
                    .as_deref()
                    .map(domain::parse_timestamp)
                    .transpose()
                    .map_err(|_| ())?,
            },
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WebRecurrence {
    every_seconds: u64,
    #[serde(default)]
    missed_policy: domain::MissedPolicy,
    max_occurrences: Option<u64>,
    end_at: Option<String>,
}

impl WebRecurrence {
    fn into_domain(self) -> Result<domain::RecurrenceSpec, ()> {
        Ok(domain::RecurrenceSpec {
            every_seconds: self.every_seconds,
            missed_policy: self.missed_policy,
            max_occurrences: self.max_occurrences,
            end_at: self
                .end_at
                .as_deref()
                .map(domain::parse_timestamp)
                .transpose()
                .map_err(|_| ())?,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WebLink {
    #[serde(rename = "type")]
    link_type: domain::LinkType,
    value: String,
}

impl From<WebLink> for LinkInput {
    fn from(value: WebLink) -> Self {
        Self {
            link_type: value.link_type,
            value: value.value,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckBody {
    project_id: String,
    task_id: Option<String>,
    session_id: Option<String>,
    task_lineage_id: Option<String>,
    lifecycle_event: domain::LifecycleEvent,
    #[serde(default)]
    active_goal_ids: Vec<String>,
    #[serde(default)]
    include_scheduled: bool,
    #[serde(default = "default_limit")]
    limit: usize,
    cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompleteBody {
    expected_version: u64,
    evidence: WebEvidence,
    completion_note: Option<String>,
    idempotency_key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WebEvidence {
    #[serde(rename = "type")]
    evidence_type: domain::EvidenceType,
    summary: String,
    reference_uri: Option<String>,
    content_hash: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    observed_at: time::OffsetDateTime,
    metadata: Option<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnoozeBody {
    expected_version: u64,
    #[serde(with = "time::serde::rfc3339")]
    snooze_until: time::OffsetDateTime,
    reason: String,
    idempotency_key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelBody {
    expected_version: u64,
    reason: String,
    idempotency_key: String,
}

#[derive(Default)]
enum PatchValue<T> {
    #[default]
    Unset,
    Null,
    Value(T),
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for PatchValue<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Option::<T>::deserialize(deserializer).map(|value| match value {
            Some(value) => Self::Value(value),
            None => Self::Null,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateBody {
    expected_version: u64,
    message: Option<String>,
    #[serde(default)]
    instructions: PatchValue<String>,
    priority: Option<Priority>,
    trigger: Option<WebTrigger>,
    #[serde(default)]
    recurrence: PatchValue<WebRecurrence>,
    overdue_after_seconds: Option<u64>,
    links: Option<Vec<WebLink>>,
    occurrence_disposition: Option<domain::OccurrenceDisposition>,
    reason: String,
    idempotency_key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    project_id: Option<String>,
    task_id: Option<String>,
    states: Option<String>,
    trigger_types: Option<String>,
    linked_goal_id: Option<String>,
    linked_memory_hash: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
    cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryQuery {
    after_sequence: Option<i64>,
    event_types: Option<String>,
    #[serde(default = "default_history_limit")]
    limit: usize,
    cursor: Option<String>,
}

const fn default_limit() -> usize {
    50
}

const fn default_history_limit() -> usize {
    100
}

async fn add(
    State(state): State<WebApiState>,
    headers: HeaderMap,
    Json(body): Json<AddBody>,
) -> Response {
    let actor = match authorize_mutation(&state, &headers, &Method::POST) {
        Ok(actor) => actor,
        Err(response) => return *response,
    };
    let trigger = match body.trigger.into_domain() {
        Ok(trigger) => trigger,
        Err(()) => return service_error(&headers, domain::ServiceError::Validation),
    };
    let recurrence = match body.recurrence.map(WebRecurrence::into_domain).transpose() {
        Ok(recurrence) => recurrence,
        Err(()) => return service_error(&headers, domain::ServiceError::Validation),
    };
    let request = AddRequest {
        project_id: body.project_id,
        task_id: body.task_id,
        message: body.message,
        instructions: body.instructions,
        priority: body.priority.into(),
        trigger,
        recurrence,
        overdue_after_seconds: body.overdue_after_seconds,
        links: body.links.into_iter().map(Into::into).collect(),
        session_id: body.session_id,
        task_lineage_id: body.task_lineage_id,
        idempotency_key: body.idempotency_key,
    };
    match state.service().add(&actor, request).await {
        Ok(result) => success(&headers, result),
        Err(error) => service_error(&headers, error),
    }
}

async fn check(
    State(state): State<WebApiState>,
    headers: HeaderMap,
    Json(body): Json<CheckBody>,
) -> Response {
    let actor = match authorize_mutation(&state, &headers, &Method::POST) {
        Ok(actor) => actor,
        Err(response) => return *response,
    };
    let request = CheckRequest {
        project_id: body.project_id,
        task_id: body.task_id,
        session_id: body.session_id,
        task_lineage_id: body.task_lineage_id,
        lifecycle_event: body.lifecycle_event,
        active_goal_ids: body.active_goal_ids,
        include_scheduled: body.include_scheduled,
        limit: body.limit,
        cursor: body.cursor,
    };
    match state.service().check(&actor, request).await {
        Ok(result) => {
            let items = result
                .items
                .into_iter()
                .map(|item| {
                    json!({
                        "remindi_id": item.remindi.id,
                        "readiness": item.readiness,
                        "message": item.remindi.message,
                        "occurrence_no": item.remindi.occurrence_no,
                        "version": item.remindi.version
                    })
                })
                .collect::<Vec<_>>();
            success(
                &headers,
                json!({
                    "checked_at": domain::canonical_timestamp(result.checked_at).ok(),
                    "items": items,
                    "next_cursor": result.next_cursor
                }),
            )
        }
        Err(error) => service_error(&headers, error),
    }
}

async fn list(
    State(state): State<WebApiState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Response {
    let actor = match actor(&state, &headers) {
        Ok(actor) => actor,
        Err(response) => return *response,
    };
    let states = match comma_values::<RemindiState>(query.states.as_deref()) {
        Ok(values) => values,
        Err(()) => return service_error(&headers, domain::ServiceError::Validation),
    };
    let request = ListRequest {
        project_id: query.project_id,
        task_id: query.task_id,
        states,
        trigger_types: comma_strings(query.trigger_types.as_deref()),
        linked_goal_id: query.linked_goal_id,
        linked_memory_hash: query.linked_memory_hash,
        limit: query.limit,
        cursor: query.cursor,
    };
    match state.service().list(&actor, request).await {
        Ok(page) => success(
            &headers,
            json!({"items": page.items, "next_cursor": page.next_cursor}),
        ),
        Err(error) => service_error(&headers, error),
    }
}

async fn detail(
    State(state): State<WebApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let actor = match actor(&state, &headers) {
        Ok(actor) => actor,
        Err(response) => return *response,
    };
    let mut cursor = None;
    loop {
        let request = ListRequest {
            limit: 200,
            cursor,
            ..ListRequest::default()
        };
        match state.service().list(&actor, request).await {
            Ok(page) => {
                if let Some(item) = page.items.into_iter().find(|item| item.id == id) {
                    return success(&headers, item);
                }
                match page.next_cursor {
                    Some(next) => cursor = Some(next),
                    None => return service_error(&headers, domain::ServiceError::NotFound),
                }
            }
            Err(error) => return service_error(&headers, error),
        }
    }
}

async fn complete(
    State(state): State<WebApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<CompleteBody>,
) -> Response {
    let actor = match authorize_mutation(&state, &headers, &Method::POST) {
        Ok(actor) => actor,
        Err(response) => return *response,
    };
    let request = CompleteRequest {
        remindi_id: id,
        expected_version: body.expected_version,
        evidence: EvidenceInput {
            evidence_type: body.evidence.evidence_type,
            summary: body.evidence.summary,
            reference_uri: body.evidence.reference_uri,
            content_hash: body.evidence.content_hash,
            observed_at: body.evidence.observed_at,
            metadata: body.evidence.metadata,
            source: EvidenceSource::AuthenticatedActor,
        },
        completion_note: body.completion_note,
        idempotency_key: body.idempotency_key,
    };
    match state
        .service()
        .complete(&actor, request, Duration::from_secs(300))
        .await
    {
        Ok(result) => success(&headers, result),
        Err(error) => service_error(&headers, error),
    }
}

async fn snooze(
    State(state): State<WebApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<SnoozeBody>,
) -> Response {
    let actor = match authorize_mutation(&state, &headers, &Method::POST) {
        Ok(actor) => actor,
        Err(response) => return *response,
    };
    let request = SnoozeRequest {
        remindi_id: id,
        expected_version: body.expected_version,
        snooze_until: body.snooze_until,
        reason: body.reason,
        idempotency_key: body.idempotency_key,
    };
    match state
        .service()
        .snooze(&actor, request, Duration::from_secs(31_536_000))
        .await
    {
        Ok(result) => success(&headers, result),
        Err(error) => service_error(&headers, error),
    }
}

async fn update(
    State(state): State<WebApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateBody>,
) -> Response {
    let actor = match authorize_mutation(&state, &headers, &Method::PATCH) {
        Ok(actor) => actor,
        Err(response) => return *response,
    };
    let instructions = match body.instructions {
        PatchValue::Unset => None,
        PatchValue::Null => Some(None),
        PatchValue::Value(value) => Some(Some(value)),
    };
    let trigger = match body.trigger.map(WebTrigger::into_domain).transpose() {
        Ok(trigger) => trigger,
        Err(()) => return service_error(&headers, domain::ServiceError::Validation),
    };
    let recurrence = match body.recurrence {
        PatchValue::Unset => None,
        PatchValue::Null => Some(None),
        PatchValue::Value(value) => match value.into_domain() {
            Ok(value) => Some(Some(value)),
            Err(()) => return service_error(&headers, domain::ServiceError::Validation),
        },
    };
    let request = UpdateRequest {
        remindi_id: id,
        expected_version: body.expected_version,
        message: body.message,
        instructions,
        priority: body.priority.map(Into::into),
        trigger,
        recurrence,
        overdue_after_seconds: body.overdue_after_seconds,
        links: body
            .links
            .map(|links| links.into_iter().map(Into::into).collect()),
        occurrence_disposition: body.occurrence_disposition,
        reason: body.reason,
        idempotency_key: body.idempotency_key,
    };
    match state.service().update(&actor, request).await {
        Ok(result) => success(&headers, result),
        Err(error) => service_error(&headers, error),
    }
}

async fn cancel(
    State(state): State<WebApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<CancelBody>,
) -> Response {
    let actor = match authorize_mutation(&state, &headers, &Method::POST) {
        Ok(actor) => actor,
        Err(response) => return *response,
    };
    let request = CancelRequest {
        remindi_id: id,
        expected_version: body.expected_version,
        reason: body.reason,
        idempotency_key: body.idempotency_key,
    };
    match state.service().cancel(&actor, request).await {
        Ok(result) => success(&headers, result),
        Err(error) => service_error(&headers, error),
    }
}

async fn history(
    State(state): State<WebApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    let actor = match actor(&state, &headers) {
        Ok(actor) => actor,
        Err(response) => return *response,
    };
    let event_types = match comma_values::<domain::EventType>(query.event_types.as_deref()) {
        Ok(values) => values,
        Err(()) => return service_error(&headers, domain::ServiceError::Validation),
    };
    let request = HistoryRequest {
        remindi_id: id,
        after_sequence: query.after_sequence,
        event_types,
        limit: query.limit,
        cursor: query.cursor,
    };
    match state.service().history(&actor, request).await {
        Ok(page) => success(
            &headers,
            json!({
                "events": page.items,
                "completion_evidence": page.evidence,
                "next_cursor": page.next_cursor
            }),
        ),
        Err(error) => service_error(&headers, error),
    }
}

fn comma_strings(value: Option<&str>) -> Vec<String> {
    value
        .into_iter()
        .flat_map(|value| value.split(','))
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn comma_values<T>(value: Option<&str>) -> Result<Vec<T>, ()>
where
    T: std::str::FromStr,
{
    comma_strings(value)
        .into_iter()
        .map(|value| value.parse().map_err(|_| ()))
        .collect()
}
