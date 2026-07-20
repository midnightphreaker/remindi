use std::sync::Arc;

use rmcp::model::{Tool, ToolAnnotations};
use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use time::OffsetDateTime;

use super::{
    McpServer,
    responses::{
        CheckData, ErrorCode, ErrorResponse, HistoryData, MutationData, PageData, ToolError,
        ToolOutput,
    },
    schemas::{
        AddInput, CancelInput, CheckInput, CompleteInput, HistoryInput, ListInput, SnoozeInput,
        UpdateInput, input_schema, strip_rust_unsigned_integer_formats,
    },
    views::{CompletionEvidenceView, EventView, RemindiView},
};
use crate::remindi::{self as domain, RecurrenceSpec, ServiceError, Trigger};

pub mod add;
pub mod cancel;
pub mod check;
pub mod complete;
pub mod history;
pub mod list;
pub mod snooze;
pub mod update;

fn tool<I: JsonSchema, O: JsonSchema + 'static>(
    name: &'static str,
    title: &'static str,
    description: &'static str,
    read_only: bool,
    destructive: bool,
    idempotent: bool,
    open_world: bool,
) -> Tool {
    let schema = input_schema::<I>()
        .as_object()
        .expect("typed MCP input schema is an object")
        .clone();
    let mut tool = Tool::new(name, description, schema)
        .with_title(title)
        .with_output_schema::<O>()
        .with_annotations(
            ToolAnnotations::with_title(title)
                .read_only(read_only)
                .destructive(destructive)
                .idempotent(idempotent)
                .open_world(open_world),
        );

    if let Some(output_schema) = tool.output_schema.as_mut() {
        let mut output = Value::Object(output_schema.as_ref().clone());
        strip_rust_unsigned_integer_formats(&mut output);
        *output_schema = Arc::new(
            output
                .as_object()
                .expect("typed MCP output schema remains an object")
                .clone(),
        );
    }

    tool
}

pub(crate) fn definitions() -> Vec<Tool> {
    vec![
        tool::<AddInput, ToolOutput<MutationData>>(
            "remindi_add",
            "Add Remindi item",
            "Create one Remindi item and its initial audit event.",
            false,
            false,
            true,
            false,
        ),
        tool::<CheckInput, ToolOutput<CheckData>>(
            "remindi_check",
            "Check Remindi items",
            "Evaluate applicable items and return due or overdue work.",
            false,
            false,
            true,
            true,
        ),
        tool::<CompleteInput, ToolOutput<MutationData>>(
            "remindi_complete",
            "Complete Remindi item",
            "Complete one item with structured evidence.",
            false,
            true,
            true,
            false,
        ),
        tool::<SnoozeInput, ToolOutput<MutationData>>(
            "remindi_snooze",
            "Snooze Remindi item",
            "Move one ready item to a later check time with a reason.",
            false,
            true,
            true,
            false,
        ),
        tool::<UpdateInput, ToolOutput<MutationData>>(
            "remindi_update",
            "Update Remindi item",
            "Change mutable fields using optimistic concurrency.",
            false,
            true,
            true,
            false,
        ),
        tool::<ListInput, ToolOutput<PageData<RemindiView>>>(
            "remindi_list",
            "List Remindi items",
            "List items without evaluating triggers or changing state.",
            true,
            false,
            true,
            false,
        ),
        tool::<CancelInput, ToolOutput<MutationData>>(
            "remindi_cancel",
            "Cancel Remindi item",
            "Cancel one active item with a reason.",
            false,
            true,
            true,
            false,
        ),
        tool::<HistoryInput, ToolOutput<HistoryData<EventView, CompletionEvidenceView>>>(
            "remindi_history",
            "Get Remindi history",
            "Return ordered events and completion evidence for one item.",
            true,
            false,
            true,
            false,
        ),
    ]
}

#[derive(Debug)]
pub(crate) enum HandlerError {
    Validation,
    Service(ServiceError),
    Serialization,
}

impl From<ServiceError> for HandlerError {
    fn from(error: ServiceError) -> Self {
        Self::Service(error)
    }
}

pub(crate) fn parse<T: DeserializeOwned>(value: Value) -> Result<T, HandlerError> {
    serde_json::from_value(value).map_err(|_| HandlerError::Validation)
}

pub(crate) fn timestamp(value: &str) -> Result<OffsetDateTime, HandlerError> {
    domain::parse_timestamp(value).map_err(|_| HandlerError::Validation)
}

pub(crate) fn trigger(input: super::schemas::TriggerInput) -> Result<Trigger, HandlerError> {
    use super::schemas::TriggerInput;
    Ok(match input {
        TriggerInput::AtTime { at } => Trigger::AtTime {
            at: timestamp(&at)?,
        },
        TriggerInput::AfterElapsed { after_seconds } => Trigger::AfterElapsed { after_seconds },
        TriggerInput::Interval {
            first_at,
            every_seconds,
        } => Trigger::Interval {
            first_at: timestamp(&first_at)?,
            every_seconds,
        },
        TriggerInput::NextSession => Trigger::NextSession,
        TriggerInput::NextContinuation => Trigger::NextContinuation,
        TriggerInput::GoalActive { goal_id } => Trigger::GoalActive { goal_id },
        TriggerInput::Condition {
            adapter,
            parameters,
            poll_interval_seconds,
            manual_check_at,
        } => Trigger::Condition {
            adapter,
            parameters,
            poll_interval_seconds,
            manual_check_at: manual_check_at.as_deref().map(timestamp).transpose()?,
        },
    })
}

pub(crate) fn recurrence(
    input: super::schemas::RecurrenceInput,
) -> Result<RecurrenceSpec, HandlerError> {
    use super::schemas::MissedPolicy;
    Ok(RecurrenceSpec {
        every_seconds: input.every_seconds,
        missed_policy: match input.missed_policy {
            MissedPolicy::Coalesce => domain::MissedPolicy::Coalesce,
            MissedPolicy::CatchUp => domain::MissedPolicy::CatchUp,
            MissedPolicy::Skip => domain::MissedPolicy::Skip,
        },
        max_occurrences: input.max_occurrences,
        end_at: input.end_at.as_deref().map(timestamp).transpose()?,
    })
}

pub(crate) fn priority(input: super::schemas::Priority) -> domain::Priority {
    match input {
        super::schemas::Priority::Low => domain::Priority::Low,
        super::schemas::Priority::Normal => domain::Priority::Normal,
        super::schemas::Priority::High => domain::Priority::High,
        super::schemas::Priority::Critical => domain::Priority::Critical,
    }
}

pub(crate) fn link(input: super::schemas::LinkInput) -> domain::LinkInput {
    let link_type = match input.link_type {
        super::schemas::LinkType::Goal => domain::LinkType::Goal,
        super::schemas::LinkType::Memory => domain::LinkType::Memory,
        super::schemas::LinkType::Issue => domain::LinkType::Issue,
        super::schemas::LinkType::Url => domain::LinkType::Url,
        super::schemas::LinkType::Artifact => domain::LinkType::Artifact,
    };
    domain::LinkInput {
        link_type,
        value: input.value,
    }
}

pub(crate) fn structured<T: Serialize>(
    request_id: &str,
    data: T,
) -> Result<rmcp::model::CallToolResult, HandlerError> {
    let value = serde_json::to_value(super::responses::SuccessResponse::new(request_id, data))
        .map_err(|_| HandlerError::Serialization)?;
    Ok(rmcp::model::CallToolResult::structured(value))
}

pub(crate) fn request_id(actor: &domain::Actor) -> String {
    actor
        .request_id
        .clone()
        .unwrap_or_else(|| "request-unavailable".to_owned())
}

fn error_result(request_id: String, error: HandlerError) -> rmcp::model::CallToolResult {
    let (code, message, details) = match error {
        HandlerError::Validation => (ErrorCode::ValidationError, "Input failed validation.", None),
        HandlerError::Serialization | HandlerError::Service(ServiceError::Internal) => (
            ErrorCode::InternalError,
            "The request could not be completed.",
            None,
        ),
        HandlerError::Service(ServiceError::NotFound) => {
            (ErrorCode::NotFound, "The Remindi item was not found.", None)
        }
        HandlerError::Service(ServiceError::InvalidState) => (
            ErrorCode::InvalidState,
            "The operation is not allowed in the current state.",
            None,
        ),
        HandlerError::Service(ServiceError::VersionConflict { current_version }) => (
            ErrorCode::VersionConflict,
            "The Remindi item changed since it was read.",
            Some(json!({"current_version": current_version})),
        ),
        HandlerError::Service(ServiceError::IdempotencyKeyReused) => (
            ErrorCode::IdempotencyKeyReused,
            "The idempotency key was reused with different input.",
            None,
        ),
        HandlerError::Service(ServiceError::DatabaseBusy) => (
            ErrorCode::DatabaseBusy,
            "The database is busy; retry the request.",
            None,
        ),
        HandlerError::Service(ServiceError::MaintenanceActive) => (
            ErrorCode::MaintenanceActive,
            "Database maintenance is active; retry the request.",
            None,
        ),
        HandlerError::Service(ServiceError::Validation | ServiceError::InvalidCursor) => {
            (ErrorCode::ValidationError, "Input failed validation.", None)
        }
    };
    let mut tool_error = ToolError::new(code, message);
    if let Some(details) = details {
        tool_error = tool_error.with_details(details);
    }
    let value =
        serde_json::to_value(ErrorResponse::new(request_id, tool_error)).unwrap_or_else(|_| {
            json!({
                "ok": false,
                "request_id": "request-unavailable",
                "error": {
                    "code": "INTERNAL_ERROR",
                    "message": "The request could not be completed.",
                    "retryable": false
                }
            })
        });
    rmcp::model::CallToolResult::structured_error(value)
}

pub(crate) async fn execute(
    server: &McpServer,
    name: &str,
    arguments: Value,
) -> rmcp::model::CallToolResult {
    let actor = server.actor();
    let request_id = request_id(&actor);
    let result = match name {
        "remindi_add" => add::handle(server, &actor, arguments).await,
        "remindi_check" => check::handle(server, &actor, arguments).await,
        "remindi_complete" => complete::handle(server, &actor, arguments).await,
        "remindi_snooze" => snooze::handle(server, &actor, arguments).await,
        "remindi_update" => update::handle(server, &actor, arguments).await,
        "remindi_list" => list::handle(server, &actor, arguments).await,
        "remindi_cancel" => cancel::handle(server, &actor, arguments).await,
        "remindi_history" => history::handle(server, &actor, arguments).await,
        _ => Err(HandlerError::Validation),
    };
    result.unwrap_or_else(|error| error_result(request_id, error))
}
