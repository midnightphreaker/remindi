use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Exact public MCP tool-call inventory.
pub const TOOL_NAMES: [&str; 8] = [
    "remindi_add",
    "remindi_check",
    "remindi_complete",
    "remindi_snooze",
    "remindi_update",
    "remindi_list",
    "remindi_cancel",
    "remindi_history",
];

/// Generates the Draft 2020-12 input schema published through MCP discovery.
pub fn input_schema<T: JsonSchema>() -> Value {
    let mut value = Value::from(schemars::schema_for!(T));
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "$schema".to_owned(),
            Value::String("https://json-schema.org/draft/2020-12/schema".to_owned()),
        );
    }
    value
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    Normal,
    High,
    Critical,
}

fn default_priority() -> Priority {
    Priority::Normal
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MissedPolicy {
    #[default]
    Coalesce,
    CatchUp,
    Skip,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LinkType {
    Goal,
    Memory,
    Issue,
    Url,
    Artifact,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LinkInput {
    #[serde(rename = "type")]
    pub link_type: LinkType,
    #[schemars(length(min = 1, max = 2048))]
    pub value: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecurrenceInput {
    #[schemars(range(min = 60, max = 31_536_000))]
    pub every_seconds: u64,
    #[serde(default)]
    pub missed_policy: MissedPolicy,
    #[schemars(range(min = 1, max = 1_000_000))]
    pub max_occurrences: Option<u64>,
    #[schemars(extend("format" = "date-time"))]
    pub end_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TriggerInput {
    AtTime {
        #[schemars(extend("format" = "date-time"))]
        at: String,
    },
    AfterElapsed {
        #[schemars(range(min = 1, max = 31_536_000))]
        after_seconds: u64,
    },
    Interval {
        #[schemars(extend("format" = "date-time"))]
        first_at: String,
        #[schemars(range(min = 60, max = 31_536_000))]
        every_seconds: u64,
    },
    NextSession,
    NextContinuation,
    GoalActive {
        #[schemars(length(min = 1, max = 512))]
        goal_id: String,
    },
    Condition {
        #[schemars(extend("pattern" = "^[a-z][a-z0-9_]{0,63}$"))]
        adapter: String,
        #[schemars(extend("type" = "object"))]
        parameters: Value,
        #[schemars(range(min = 30, max = 86_400))]
        poll_interval_seconds: Option<u64>,
        #[schemars(extend("format" = "date-time"))]
        manual_check_at: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceType {
    Observation,
    TestResult,
    Artifact,
    LogReference,
    ChangeReference,
    UserConfirmation,
    ExternalReference,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "anyOf" = [
        {"required": ["reference_uri"]},
        {"required": ["content_hash"]}
    ]
))]
pub struct EvidenceInput {
    #[serde(rename = "type")]
    pub evidence_type: EvidenceType,
    #[schemars(length(min = 1, max = 4096))]
    pub summary: String,
    #[schemars(length(max = 4096), extend("format" = "uri"))]
    pub reference_uri: Option<String>,
    #[schemars(extend("pattern" = "^(sha256:)?[a-fA-F0-9]{64}$"))]
    pub content_hash: Option<String>,
    #[schemars(extend("format" = "date-time"))]
    pub observed_at: String,
    #[schemars(extend("type" = "object"))]
    pub metadata: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AddInput {
    #[schemars(length(min = 1, max = 512))]
    pub project_id: String,
    #[schemars(length(min = 1, max = 512))]
    pub task_id: Option<String>,
    #[schemars(length(min = 1, max = 8192))]
    pub message: String,
    #[schemars(length(max = 32_768))]
    pub instructions: Option<String>,
    #[serde(default = "default_priority")]
    pub priority: Priority,
    pub trigger: TriggerInput,
    pub recurrence: Option<RecurrenceInput>,
    #[serde(default)]
    #[schemars(range(min = 0, max = 31_536_000))]
    pub overdue_after_seconds: u64,
    #[serde(default)]
    #[schemars(length(max = 100), extend("uniqueItems" = true))]
    pub links: Vec<LinkInput>,
    #[schemars(length(max = 512))]
    pub session_id: Option<String>,
    #[schemars(length(max = 512))]
    pub task_lineage_id: Option<String>,
    #[schemars(length(min = 8, max = 128), extend("pattern" = "^[A-Za-z0-9._:-]+$"))]
    pub idempotency_key: String,
}

impl AddInput {
    pub fn validate_semantics(&self) -> Result<(), InputSemanticError> {
        match (&self.trigger, &self.recurrence) {
            (TriggerInput::Interval { every_seconds, .. }, Some(recurrence))
                if *every_seconds == recurrence.every_seconds =>
            {
                Ok(())
            }
            (TriggerInput::Interval { .. }, None) | (_, None) => Ok(()),
            (TriggerInput::Interval { .. }, Some(_)) => {
                Err(InputSemanticError::RecurrenceIntervalMismatch)
            }
            (_, Some(_)) => Err(InputSemanticError::RecurrenceRequiresInterval),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleEvent {
    TaskStart,
    Checkpoint,
    Continuation,
    FinalReview,
}

fn default_true() -> bool {
    true
}

fn default_check_limit() -> u16 {
    50
}

fn default_history_limit() -> u16 {
    100
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CheckInput {
    #[schemars(length(min = 1, max = 512))]
    pub project_id: String,
    #[schemars(length(min = 1, max = 512))]
    pub task_id: Option<String>,
    #[schemars(length(max = 512))]
    pub session_id: Option<String>,
    #[schemars(length(max = 512))]
    pub task_lineage_id: Option<String>,
    pub lifecycle_event: LifecycleEvent,
    #[serde(default)]
    #[schemars(length(max = 1000), inner(length(min = 1, max = 512)), extend("uniqueItems" = true))]
    pub active_goal_ids: Vec<String>,
    #[serde(default)]
    pub include_scheduled: bool,
    #[serde(default = "default_true")]
    pub evaluate_conditions: bool,
    #[serde(default = "default_check_limit")]
    #[schemars(range(min = 1, max = 200))]
    pub limit: u16,
    #[schemars(length(max = 2048))]
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CompleteInput {
    pub remindi_id: Uuid,
    #[schemars(range(min = 1))]
    pub expected_version: u64,
    pub evidence: EvidenceInput,
    #[schemars(length(max = 4096))]
    pub completion_note: Option<String>,
    #[schemars(length(min = 8, max = 128), extend("pattern" = "^[A-Za-z0-9._:-]+$"))]
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SnoozeInput {
    pub remindi_id: Uuid,
    #[schemars(range(min = 1))]
    pub expected_version: u64,
    #[schemars(extend("format" = "date-time"))]
    pub snooze_until: String,
    #[schemars(length(min = 1, max = 4096))]
    pub reason: String,
    #[schemars(length(min = 8, max = 128), extend("pattern" = "^[A-Za-z0-9._:-]+$"))]
    pub idempotency_key: String,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OccurrenceDisposition {
    Acknowledged,
    Skipped,
}

/// Three-state patch value: absent, explicit null, or a replacement value.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Patch<T> {
    #[default]
    Unset,
    Null,
    Value(T),
}

impl<T> Patch<T> {
    fn is_set(&self) -> bool {
        !matches!(self, Self::Unset)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Patch<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Option::<T>::deserialize(deserializer).map(|value| match value {
            Some(value) => Self::Value(value),
            None => Self::Null,
        })
    }
}

impl<T: JsonSchema> JsonSchema for Patch<T> {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        format!("Nullable_{}", T::schema_name()).into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        <Option<T>>::json_schema(generator)
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
#[schemars(extend("minProperties" = 5))]
pub struct UpdateInput {
    pub remindi_id: Uuid,
    #[schemars(range(min = 1))]
    pub expected_version: u64,
    #[schemars(length(min = 1, max = 8192))]
    pub message: Option<String>,
    #[serde(default)]
    #[schemars(length(max = 32_768))]
    pub instructions: Patch<String>,
    pub priority: Option<Priority>,
    pub trigger: Option<TriggerInput>,
    #[serde(default)]
    pub recurrence: Patch<RecurrenceInput>,
    #[schemars(range(min = 0, max = 31_536_000))]
    pub overdue_after_seconds: Option<u64>,
    #[schemars(length(max = 100), extend("uniqueItems" = true))]
    pub links: Option<Vec<LinkInput>>,
    pub occurrence_disposition: Option<OccurrenceDisposition>,
    #[schemars(length(min = 1, max = 4096))]
    pub reason: String,
    #[schemars(length(min = 8, max = 128), extend("pattern" = "^[A-Za-z0-9._:-]+$"))]
    pub idempotency_key: String,
}

impl UpdateInput {
    pub fn validate_semantics(&self) -> Result<(), InputSemanticError> {
        let changed = self.message.is_some()
            || self.instructions.is_set()
            || self.priority.is_some()
            || self.trigger.is_some()
            || self.recurrence.is_set()
            || self.overdue_after_seconds.is_some()
            || self.links.is_some()
            || self.occurrence_disposition.is_some();
        changed
            .then_some(())
            .ok_or(InputSemanticError::UpdateRequiresMutableField)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemindiState {
    Scheduled,
    Due,
    Overdue,
    Snoozed,
    Completed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TriggerType {
    AtTime,
    AfterElapsed,
    Interval,
    NextSession,
    NextContinuation,
    GoalActive,
    Condition,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ListInput {
    #[schemars(length(min = 1, max = 512))]
    pub project_id: Option<String>,
    #[schemars(length(min = 1, max = 512))]
    pub task_id: Option<String>,
    #[serde(default)]
    #[schemars(extend("uniqueItems" = true))]
    pub states: Vec<RemindiState>,
    #[serde(default)]
    #[schemars(extend("uniqueItems" = true))]
    pub trigger_types: Vec<TriggerType>,
    #[schemars(length(max = 512))]
    pub linked_goal_id: Option<String>,
    #[schemars(length(max = 512))]
    pub linked_memory_hash: Option<String>,
    #[serde(default = "default_check_limit")]
    #[schemars(range(min = 1, max = 200))]
    pub limit: u16,
    #[schemars(length(max = 2048))]
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CancelInput {
    pub remindi_id: Uuid,
    #[schemars(range(min = 1))]
    pub expected_version: u64,
    #[schemars(length(min = 1, max = 4096))]
    pub reason: String,
    #[schemars(length(min = 8, max = 128), extend("pattern" = "^[A-Za-z0-9._:-]+$"))]
    pub idempotency_key: String,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    Created,
    Checked,
    BecameDue,
    BecameOverdue,
    ConditionEvaluated,
    OccurrenceAdvanced,
    Snoozed,
    Updated,
    Completed,
    Cancelled,
    DeliveryAttempted,
    DeliverySucceeded,
    DeliveryFailed,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HistoryInput {
    pub remindi_id: Uuid,
    #[schemars(range(min = 0))]
    pub after_sequence: Option<u64>,
    #[serde(default)]
    #[schemars(extend("uniqueItems" = true))]
    pub event_types: Vec<EventType>,
    #[serde(default = "default_history_limit")]
    #[schemars(range(min = 1, max = 200))]
    pub limit: u16,
    #[schemars(length(max = 2048))]
    pub cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputSemanticError {
    RecurrenceRequiresInterval,
    RecurrenceIntervalMismatch,
    UpdateRequiresMutableField,
}

impl fmt::Display for InputSemanticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::RecurrenceRequiresInterval => "recurrence requires an interval trigger",
            Self::RecurrenceIntervalMismatch => {
                "recurrence interval must match the interval trigger"
            }
            Self::UpdateRequiresMutableField => "at least one mutable field is required",
        })
    }
}

impl std::error::Error for InputSemanticError {}
