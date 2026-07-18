//! Composable authenticated administration API routes.
//!
//! The shared router must attach WebUI authentication, same-origin, CSRF, and
//! [`AdminApiContext`] before merging these routes.

use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path, Query, State},
    http::{
        HeaderValue, StatusCode,
        header::{CONTENT_DISPOSITION, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio_util::io::ReaderStream;

use crate::admin::{
    AdminActor, AdminError, AdminService, BootstrapView,
    adapters::{AdapterConfigView, AdapterConfiguration},
    audit::AdminEvent,
    backup::{BackupError, BackupManager, BackupRecord, BackupSource},
    settings::RuntimeSetting,
};

/// State owned by the authenticated administration route group.
#[derive(Clone)]
pub struct AdminApiState {
    service: Arc<AdminService>,
    backups: Option<Arc<BackupManager>>,
}

impl AdminApiState {
    #[must_use]
    pub fn new(service: Arc<AdminService>) -> Self {
        Self {
            service,
            backups: None,
        }
    }

    #[must_use]
    pub fn with_backups(mut self, backups: Arc<BackupManager>) -> Self {
        self.backups = Some(backups);
        self
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
    error: ApiFailure,
    request_id: String,
}

enum ApiFailure {
    Admin(AdminError),
    Backup(BackupError),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message, retryable) = match self.error {
            ApiFailure::Admin(AdminError::Validation) => (
                StatusCode::BAD_REQUEST,
                "VALIDATION_ERROR",
                "Administrative input failed validation.",
                false,
            ),
            ApiFailure::Admin(AdminError::VersionConflict) => (
                StatusCode::CONFLICT,
                "VERSION_CONFLICT",
                "The expected administrative version is stale.",
                true,
            ),
            ApiFailure::Backup(BackupError::Invalid) => (
                StatusCode::BAD_REQUEST,
                "BACKUP_INVALID",
                "The backup failed verification.",
                false,
            ),
            ApiFailure::Backup(BackupError::LimitExceeded) => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "LIMIT_EXCEEDED",
                "The backup upload exceeded its configured limit.",
                false,
            ),
            ApiFailure::Backup(BackupError::NotFound) => (
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                "The requested backup was not found.",
                false,
            ),
            ApiFailure::Admin(AdminError::Database)
            | ApiFailure::Backup(BackupError::Database | BackupError::Io) => (
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
        .route("/backups", get(backups).post(create_backup))
        .route(
            "/backups/upload",
            post(upload_backup).layer(DefaultBodyLimit::disable()),
        )
        .route("/backups/{id}/verify", post(verify_backup))
        .route("/backups/{id}/download", get(download_backup))
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
        error: ApiFailure::Admin(error),
        request_id: context.request_id.clone(),
    }
}

async fn backups(
    State(state): State<AdminApiState>,
    Extension(context): Extension<AdminApiContext>,
) -> Result<Json<Success<Vec<BackupRecord>>>, ApiError> {
    let data = backup_manager(&state, &context)?
        .list()
        .await
        .map_err(|error| backup_api_error(error, &context))?;
    Ok(Json(Success {
        ok: true,
        request_id: context.request_id,
        data,
    }))
}

async fn create_backup(
    State(state): State<AdminApiState>,
    Extension(context): Extension<AdminApiContext>,
) -> Result<(StatusCode, Json<Success<BackupRecord>>), ApiError> {
    let data = backup_manager(&state, &context)?
        .create(BackupSource::Manual, &context.actor)
        .await
        .map_err(|error| backup_api_error(error, &context))?;
    Ok((
        StatusCode::CREATED,
        Json(Success {
            ok: true,
            request_id: context.request_id,
            data,
        }),
    ))
}

async fn upload_backup(
    State(state): State<AdminApiState>,
    Extension(context): Extension<AdminApiContext>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<Success<BackupRecord>>), ApiError> {
    let maximum = state
        .service
        .runtime_settings()
        .await
        .map_err(|error| api_error(error, &context))?
        .into_iter()
        .find(|setting| setting.key == "backups.upload_max_bytes")
        .and_then(|setting| u64::try_from(setting.value).ok())
        .ok_or_else(|| api_error(AdminError::Database, &context))?;
    let field = multipart
        .next_field()
        .await
        .map_err(|_| backup_api_error(BackupError::Invalid, &context))?
        .filter(|field| {
            field.name() == Some("file")
                && matches!(
                    field.content_type(),
                    Some("application/vnd.sqlite3" | "application/x-sqlite3")
                )
        })
        .ok_or_else(|| backup_api_error(BackupError::Invalid, &context))?;
    let data = backup_manager(&state, &context)?
        .upload(
            field.map(|chunk| chunk.map_err(|_| ())),
            maximum,
            &context.actor,
        )
        .await
        .map_err(|error| backup_api_error(error, &context))?;
    Ok((
        StatusCode::CREATED,
        Json(Success {
            ok: true,
            request_id: context.request_id,
            data,
        }),
    ))
}

async fn download_backup(
    State(state): State<AdminApiState>,
    Extension(context): Extension<AdminApiContext>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let (record, path) = backup_manager(&state, &context)?
        .download(&id)
        .await
        .map_err(|error| backup_api_error(error, &context))?;
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| backup_api_error(BackupError::Io, &context))?;
    let disposition =
        HeaderValue::from_str(&format!("attachment; filename=\"{}\"", record.file_name))
            .map_err(|_| backup_api_error(BackupError::Database, &context))?;
    let mut response = Response::new(Body::from_stream(ReaderStream::new(file)));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/vnd.sqlite3"),
    );
    response
        .headers_mut()
        .insert(CONTENT_DISPOSITION, disposition);
    response.headers_mut().insert(
        "digest",
        HeaderValue::from_str(&format!("sha-256={}", record.sha256))
            .map_err(|_| backup_api_error(BackupError::Database, &context))?,
    );
    Ok(response)
}

async fn verify_backup(
    State(state): State<AdminApiState>,
    Extension(context): Extension<AdminApiContext>,
    Path(id): Path<String>,
) -> Result<Json<Success<BackupRecord>>, ApiError> {
    let data = backup_manager(&state, &context)?
        .verify(&id, &context.actor)
        .await
        .map_err(|error| backup_api_error(error, &context))?;
    Ok(Json(Success {
        ok: true,
        request_id: context.request_id,
        data,
    }))
}

fn backup_manager<'a>(
    state: &'a AdminApiState,
    context: &AdminApiContext,
) -> Result<&'a BackupManager, ApiError> {
    state
        .backups
        .as_deref()
        .ok_or_else(|| backup_api_error(BackupError::Database, context))
}

fn backup_api_error(error: BackupError, context: &AdminApiContext) -> ApiError {
    ApiError {
        error: ApiFailure::Backup(error),
        request_id: context.request_id.clone(),
    }
}
