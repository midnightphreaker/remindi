use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use super::schemas::RemindiState;

/// Successful structured tool result.
#[derive(Clone, Debug, JsonSchema, Serialize)]
pub struct SuccessResponse<T> {
    #[schemars(extend("const" = true))]
    ok: bool,
    pub request_id: String,
    pub data: T,
}

impl<T> SuccessResponse<T> {
    pub fn new(request_id: impl Into<String>, data: T) -> Self {
        Self {
            ok: true,
            request_id: request_id.into(),
            data,
        }
    }
}

/// Failed structured tool result.
#[derive(Clone, Debug, JsonSchema, Serialize)]
pub struct ErrorResponse {
    #[schemars(extend("const" = false))]
    ok: bool,
    pub request_id: String,
    pub error: ToolError,
}

impl ErrorResponse {
    pub fn new(request_id: impl Into<String>, error: ToolError) -> Self {
        Self {
            ok: false,
            request_id: request_id.into(),
            error,
        }
    }
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
pub struct ToolError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// Common MCP output schema. Runtime results contain exactly one of `data` or `error`.
#[derive(Clone, Debug, JsonSchema, Serialize)]
#[schemars(extend(
    "oneOf" = [
        {"properties": {"ok": {"const": true}}, "required": ["ok", "request_id", "data"]},
        {"properties": {"ok": {"const": false}}, "required": ["ok", "request_id", "error"]}
    ]
))]
pub struct ToolOutput<T> {
    pub ok: bool,
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ToolError>,
}

impl ToolError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            retryable: matches!(code.retryable(), Retryability::Always),
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }
}

#[derive(Clone, Copy, Debug, JsonSchema, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    ValidationError,
    Unauthenticated,
    Forbidden,
    NotFound,
    InvalidState,
    VersionConflict,
    IdempotencyKeyReused,
    DatabaseBusy,
    AdapterNotFound,
    AdapterDisabled,
    AdapterTimeout,
    AdapterError,
    ReauthenticationRequired,
    CsrfRejected,
    WorkloadConflict,
    MaintenanceActive,
    BackupInvalid,
    RestoreFailed,
    LimitExceeded,
    InternalError,
}

impl ErrorCode {
    pub const fn retryable(self) -> Retryability {
        match self {
            Self::VersionConflict
            | Self::DatabaseBusy
            | Self::AdapterTimeout
            | Self::WorkloadConflict
            | Self::MaintenanceActive => Retryability::Always,
            Self::AdapterError | Self::RestoreFailed | Self::InternalError => {
                Retryability::Conditional
            }
            Self::ValidationError
            | Self::Unauthenticated
            | Self::Forbidden
            | Self::NotFound
            | Self::InvalidState
            | Self::IdempotencyKeyReused
            | Self::AdapterNotFound
            | Self::AdapterDisabled
            | Self::ReauthenticationRequired
            | Self::CsrfRejected
            | Self::BackupInvalid
            | Self::LimitExceeded => Retryability::Never,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retryability {
    Never,
    Always,
    Conditional,
}

impl Retryability {
    pub const fn is_conditional(self) -> bool {
        matches!(self, Self::Conditional)
    }
}

impl From<bool> for Retryability {
    fn from(value: bool) -> Self {
        if value { Self::Always } else { Self::Never }
    }
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
pub struct RemindiVersion {
    pub id: Uuid,
    pub state: RemindiState,
    pub version: u64,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
pub struct MutationData {
    pub remindi: RemindiVersion,
}

pub type AddResponse = SuccessResponse<MutationData>;
pub type CompleteResponse = SuccessResponse<MutationData>;
pub type SnoozeResponse = SuccessResponse<MutationData>;
pub type UpdateResponse = SuccessResponse<MutationData>;
pub type CancelResponse = SuccessResponse<MutationData>;

#[derive(Clone, Debug, JsonSchema, Serialize)]
pub struct ReadyItem {
    pub remindi_id: Uuid,
    pub readiness: Readiness,
    pub message: String,
    pub occurrence_no: u64,
    pub version: u64,
}

#[derive(Clone, Copy, Debug, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    Due,
    Overdue,
    ManualVerification,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
pub struct CheckData {
    #[schemars(extend("format" = "date-time"))]
    pub checked_at: String,
    pub items: Vec<ReadyItem>,
    pub next_cursor: Option<String>,
}

pub type CheckResponse = SuccessResponse<CheckData>;

#[derive(Clone, Debug, JsonSchema, Serialize)]
pub struct PageData<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

pub type ListResponse<T> = SuccessResponse<PageData<T>>;

#[derive(Clone, Debug, JsonSchema, Serialize)]
pub struct HistoryData<E, C> {
    pub events: Vec<E>,
    pub completion_evidence: Vec<C>,
    pub next_cursor: Option<String>,
}

pub type HistoryResponse<E, C> = SuccessResponse<HistoryData<E, C>>;
