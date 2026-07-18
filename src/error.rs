use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::{Map, Value};
use thiserror::Error;

use crate::http::middleware::RequestId;

/// Stable public error codes from the Remindi contract.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    /// Input failed validation.
    ValidationError,
    /// Actor identity is absent or invalid.
    Unauthenticated,
    /// Actor lacks access.
    Forbidden,
    /// The requested resource is absent or hidden.
    NotFound,
    /// The operation is invalid for the current state.
    InvalidState,
    /// The expected version is stale.
    VersionConflict,
    /// An idempotency key was reused for different input.
    IdempotencyKeyReused,
    /// SQLite did not obtain its lock before the deadline.
    DatabaseBusy,
    /// The requested adapter is not registered.
    AdapterNotFound,
    /// The requested adapter is disabled.
    AdapterDisabled,
    /// An adapter exceeded its deadline.
    AdapterTimeout,
    /// An adapter failed safely.
    AdapterError,
    /// A sensitive action requires recent password verification.
    ReauthenticationRequired,
    /// Browser same-origin or CSRF validation failed.
    CsrfRejected,
    /// Another workload lifecycle operation is active.
    WorkloadConflict,
    /// The database is temporarily quiesced.
    MaintenanceActive,
    /// A backup failed validation.
    BackupInvalid,
    /// Restore failed and rollback was attempted.
    RestoreFailed,
    /// A configured or request limit was exceeded.
    LimitExceeded,
    /// An unexpected internal error occurred.
    InternalError,
}

impl ErrorCode {
    /// Returns the fixed retryability default for the error code.
    #[must_use]
    pub const fn retryable(self) -> bool {
        matches!(
            self,
            Self::VersionConflict
                | Self::DatabaseBusy
                | Self::AdapterTimeout
                | Self::WorkloadConflict
                | Self::MaintenanceActive
        )
    }
}

/// Safe HTTP error response with the common JSON envelope.
pub struct AppError {
    status: StatusCode,
    request_id: RequestId,
    code: ErrorCode,
    message: &'static str,
    details: Map<String, Value>,
}

impl AppError {
    /// Creates a minimally revealing missing-resource response.
    #[must_use]
    pub fn not_found(request_id: RequestId) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            request_id,
            code: ErrorCode::NotFound,
            message: "The requested resource was not found.",
            details: Map::new(),
        }
    }
}

#[derive(Serialize)]
struct ErrorEnvelope {
    ok: bool,
    request_id: String,
    error: ErrorBody,
}

#[derive(Serialize)]
struct ErrorBody {
    code: ErrorCode,
    message: &'static str,
    retryable: bool,
    details: Map<String, Value>,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = ErrorEnvelope {
            ok: false,
            request_id: self.request_id.into_inner(),
            error: ErrorBody {
                code: self.code,
                message: self.message,
                retryable: self.code.retryable(),
                details: self.details,
            },
        };

        (self.status, Json(body)).into_response()
    }
}

/// Errors that may occur while installing the process-wide JSON logger.
#[derive(Debug, Error)]
pub enum TracingError {
    /// The configured filtering directive was malformed.
    #[error("REMINDI_LOG_LEVEL is not a valid tracing filter")]
    InvalidFilter(#[from] tracing_subscriber::filter::ParseError),
    /// Another component already installed a global tracing subscriber.
    #[error("the tracing subscriber is already configured")]
    AlreadyConfigured(#[from] tracing_subscriber::util::TryInitError),
}
