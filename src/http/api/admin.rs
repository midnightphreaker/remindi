//! Composable authenticated administration API routes.
//!
//! The shared router must attach WebUI authentication, same-origin, CSRF, and
//! [`AdminApiContext`] before merging these routes.

use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, patch},
};
use serde::{Deserialize, Serialize};

use crate::admin::{
    AdminActor, AdminError, AdminService, BootstrapView,
    adapters::{AdapterConfigView, AdapterConfiguration},
    audit::AdminEvent,
    settings::RuntimeSetting,
};

/// State owned by the authenticated administration route group.
#[derive(Clone)]
pub struct AdminApiState {
    service: Arc<AdminService>,
}

impl AdminApiState {
    #[must_use]
    pub fn new(service: Arc<AdminService>) -> Self {
        Self { service }
    }
}

/// Authenticated actor and deterministic request ID inserted by WebUI middleware.
#[derive(Clone, Debug)]
pub struct AdminApiContext {
    actor: AdminActor,
    request_id: String,
}

impl AdminApiContext {
    pub fn new(actor_id: impl Into<String>, request_id: String) -> Result<Self, AdminError> {
        let actor = AdminActor::new(actor_id, Some(request_id.clone()))?;
        Ok(Self { actor, request_id })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateRuntimeSettingRequest {
    pub value: i64,
    pub expected_version: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateAdapterRequest {
    pub enabled: bool,
    pub configuration: AdapterConfiguration,
    pub expected_version: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditQuery {
    pub after_sequence: Option<i64>,
    pub limit: Option<u16>,
}

#[derive(Serialize)]
struct Success<T> {
    ok: bool,
    request_id: String,
    data: T,
}

#[derive(Serialize)]
struct ErrorEnvelope {
    ok: bool,
    request_id: String,
    error: ErrorBody,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
    retryable: bool,
}

struct ApiError {
    error: AdminError,
    request_id: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message, retryable) = match self.error {
            AdminError::Validation => (
                StatusCode::BAD_REQUEST,
                "VALIDATION_ERROR",
                "Administrative input failed validation.",
                false,
            ),
            AdminError::VersionConflict => (
                StatusCode::CONFLICT,
                "VERSION_CONFLICT",
                "The expected administrative version is stale.",
                true,
            ),
            AdminError::Database => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "The administrative operation failed.",
                false,
            ),
        };
        (
            status,
            Json(ErrorEnvelope {
                ok: false,
                request_id: self.request_id,
                error: ErrorBody {
                    code,
                    message,
                    retryable,
                },
            }),
        )
            .into_response()
    }
}

/// Returns routes for integration below `/api/v1`.
pub fn routes() -> Router<AdminApiState> {
    Router::new()
        .route("/settings/bootstrap", get(bootstrap))
        .route("/settings", get(runtime_settings))
        .route("/settings/{key}", patch(update_runtime_setting))
        .route("/adapters", get(adapter_configs))
        .route("/adapters/{name}", patch(update_adapter))
        .route("/admin-events", get(admin_events))
}

async fn bootstrap(
    State(state): State<AdminApiState>,
    Extension(context): Extension<AdminApiContext>,
) -> Json<Success<BootstrapView>> {
    Json(Success {
        ok: true,
        request_id: context.request_id,
        data: state.service.bootstrap_view(),
    })
}

async fn runtime_settings(
    State(state): State<AdminApiState>,
    Extension(context): Extension<AdminApiContext>,
) -> Result<Json<Success<Vec<RuntimeSetting>>>, ApiError> {
    let data = state
        .service
        .runtime_settings()
        .await
        .map_err(|error| api_error(error, &context))?;
    Ok(Json(Success {
        ok: true,
        request_id: context.request_id,
        data,
    }))
}

async fn update_runtime_setting(
    State(state): State<AdminApiState>,
    Extension(context): Extension<AdminApiContext>,
    Path(key): Path<String>,
    Json(request): Json<UpdateRuntimeSettingRequest>,
) -> Result<Json<Success<RuntimeSetting>>, ApiError> {
    let data = state
        .service
        .update_runtime_setting(
            &key,
            request.value,
            request.expected_version,
            &context.actor,
        )
        .await
        .map_err(|error| api_error(error, &context))?;
    Ok(Json(Success {
        ok: true,
        request_id: context.request_id,
        data,
    }))
}

async fn adapter_configs(
    State(state): State<AdminApiState>,
    Extension(context): Extension<AdminApiContext>,
) -> Result<Json<Success<Vec<AdapterConfigView>>>, ApiError> {
    let data = state
        .service
        .adapter_configs()
        .await
        .map_err(|error| api_error(error, &context))?;
    Ok(Json(Success {
        ok: true,
        request_id: context.request_id,
        data,
    }))
}

async fn update_adapter(
    State(state): State<AdminApiState>,
    Extension(context): Extension<AdminApiContext>,
    Path(name): Path<String>,
    Json(request): Json<UpdateAdapterRequest>,
) -> Result<Json<Success<AdapterConfigView>>, ApiError> {
    let data = state
        .service
        .update_adapter(
            &name,
            request.enabled,
            request.configuration,
            request.expected_version,
            &context.actor,
        )
        .await
        .map_err(|error| api_error(error, &context))?;
    Ok(Json(Success {
        ok: true,
        request_id: context.request_id,
        data,
    }))
}

async fn admin_events(
    State(state): State<AdminApiState>,
    Extension(context): Extension<AdminApiContext>,
    Query(query): Query<AuditQuery>,
) -> Result<Json<Success<Vec<AdminEvent>>>, ApiError> {
    let data = state
        .service
        .admin_events(query.after_sequence, query.limit.unwrap_or(100))
        .await
        .map_err(|error| api_error(error, &context))?;
    Ok(Json(Success {
        ok: true,
        request_id: context.request_id,
        data,
    }))
}

fn api_error(error: AdminError, context: &AdminApiContext) -> ApiError {
    ApiError {
        error,
        request_id: context.request_id.clone(),
    }
}
