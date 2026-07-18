//! Authenticated administration API routes below `/api/v1`.

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path, Query, State},
    http::{
        HeaderMap, HeaderValue, Method, StatusCode,
        header::{CONTENT_DISPOSITION, CONTENT_TYPE},
    },
    response::Response,
    routing::{get, patch, post},
};
use futures::StreamExt;
use serde::Deserialize;
use time::{Duration, OffsetDateTime};
use tokio_util::io::ReaderStream;

use crate::admin::{
    AdminActor, AdminError,
    adapters::AdapterConfiguration,
    backup::{BackupError, BackupRecord, BackupSource, RestoreFault, RestoreOutcome},
    workloads::{WorkloadAction, WorkloadComponent, WorkloadError},
};
use crate::auth::web_session::WebMode;

use super::{WebApiState, actor, api_error, authorize_mutation, success};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateRuntimeSettingRequest {
    value: i64,
    expected_version: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateAdapterRequest {
    enabled: bool,
    configuration: AdapterConfiguration,
    expected_version: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditQuery {
    after_sequence: Option<i64>,
    limit: Option<u16>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkloadMutation {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoreRequest {
    confirmation: String,
}

/// Returns the complete administration route group for a configured WebUI API.
pub fn routes() -> Router<WebApiState> {
    Router::new()
        .route("/settings/bootstrap", get(bootstrap))
        .route("/settings", get(runtime_settings))
        .route("/settings/{key}", patch(update_runtime_setting))
        .route("/adapters", get(adapter_configs))
        .route("/adapters/{name}", patch(update_adapter))
        .route("/workloads", get(workloads))
        .route("/workloads/{component}/{action}", post(transition_workload))
        .route("/admin-events", get(admin_events))
        .route("/backups", get(backups).post(create_backup))
        .route(
            "/backups/upload",
            post(upload_backup).layer(DefaultBodyLimit::disable()),
        )
        .route("/backups/{id}/verify", post(verify_backup))
        .route("/backups/{id}/download", get(download_backup))
        .route("/backups/{id}/restore", post(restore_backup))
}

async fn bootstrap(State(state): State<WebApiState>, headers: HeaderMap) -> Response {
    if let Err(response) = actor(&state, &headers) {
        return *response;
    }
    let Some(service) = state.administration() else {
        return unavailable(&headers);
    };
    success(&headers, service.bootstrap_view())
}

async fn runtime_settings(State(state): State<WebApiState>, headers: HeaderMap) -> Response {
    if let Err(response) = actor(&state, &headers) {
        return *response;
    }
    let Some(service) = state.administration() else {
        return unavailable(&headers);
    };
    match service.runtime_settings().await {
        Ok(data) => success(&headers, data),
        Err(error) => admin_error(&headers, error),
    }
}

async fn update_runtime_setting(
    State(state): State<WebApiState>,
    headers: HeaderMap,
    Path(key): Path<String>,
    Json(request): Json<UpdateRuntimeSettingRequest>,
) -> Response {
    let actor = match admin_actor(&state, &headers, &Method::PATCH) {
        Ok(actor) => actor,
        Err(response) => return *response,
    };
    let Some(service) = state.administration() else {
        return unavailable(&headers);
    };
    match service
        .update_runtime_setting(&key, request.value, request.expected_version, &actor)
        .await
    {
        Ok(data) => success(&headers, data),
        Err(error) => admin_error(&headers, error),
    }
}

async fn adapter_configs(State(state): State<WebApiState>, headers: HeaderMap) -> Response {
    if let Err(response) = actor(&state, &headers) {
        return *response;
    }
    let Some(service) = state.administration() else {
        return unavailable(&headers);
    };
    match service.adapter_configs().await {
        Ok(data) => success(&headers, data),
        Err(error) => admin_error(&headers, error),
    }
}

async fn update_adapter(
    State(state): State<WebApiState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(request): Json<UpdateAdapterRequest>,
) -> Response {
    let actor = match admin_actor(&state, &headers, &Method::PATCH) {
        Ok(actor) => actor,
        Err(response) => return *response,
    };
    let Some(service) = state.administration() else {
        return unavailable(&headers);
    };
    match service
        .update_adapter(
            &name,
            request.enabled,
            request.configuration,
            request.expected_version,
            &actor,
        )
        .await
    {
        Ok(data) => success(&headers, data),
        Err(error) => admin_error(&headers, error),
    }
}

async fn workloads(State(state): State<WebApiState>, headers: HeaderMap) -> Response {
    if let Err(response) = actor(&state, &headers) {
        return *response;
    }
    let Some(workloads) = state.workloads() else {
        return unavailable(&headers);
    };
    success(&headers, workloads.status())
}

async fn transition_workload(
    State(state): State<WebApiState>,
    headers: HeaderMap,
    Path((component, action)): Path<(WorkloadComponent, WorkloadAction)>,
    Json(_request): Json<WorkloadMutation>,
) -> Response {
    let actor = match admin_actor(&state, &headers, &Method::POST) {
        Ok(actor) => actor,
        Err(response) => return *response,
    };
    let (Some(service), Some(workloads)) = (state.administration(), state.workloads()) else {
        return unavailable(&headers);
    };
    let result = workloads
        .transition(component, action, actor.actor_id(), actor.request_id())
        .await;
    let failure_code = match &result {
        Ok(_) => None,
        Err(WorkloadError::TransitionConflict) => Some("WORKLOAD_CONFLICT"),
        Err(_) => Some("INTERNAL_ERROR"),
    };
    if let Err(error) = service
        .audit_workload_action(component, action, &actor, failure_code)
        .await
    {
        return admin_error(&headers, error);
    }
    match result {
        Ok(data) => success(&headers, data),
        Err(WorkloadError::TransitionConflict) => api_error(
            &headers,
            StatusCode::CONFLICT,
            "WORKLOAD_CONFLICT",
            "Another workload lifecycle operation is active.",
            true,
            None,
        ),
        Err(_) => api_error(
            &headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            "The workload transition failed.",
            false,
            None,
        ),
    }
}

async fn admin_events(
    State(state): State<WebApiState>,
    headers: HeaderMap,
    Query(query): Query<AuditQuery>,
) -> Response {
    if let Err(response) = actor(&state, &headers) {
        return *response;
    }
    let Some(service) = state.administration() else {
        return unavailable(&headers);
    };
    match service
        .admin_events(query.after_sequence, query.limit.unwrap_or(100))
        .await
    {
        Ok(data) => success(&headers, data),
        Err(error) => admin_error(&headers, error),
    }
}

async fn backups(State(state): State<WebApiState>, headers: HeaderMap) -> Response {
    if let Err(response) = actor(&state, &headers) {
        return *response;
    }
    let Some(backups) = state.backups() else {
        return unavailable(&headers);
    };
    match backups.list().await {
        Ok(data) => success(&headers, data),
        Err(error) => backup_error(&headers, error),
    }
}

async fn create_backup(State(state): State<WebApiState>, headers: HeaderMap) -> Response {
    let actor = match admin_actor(&state, &headers, &Method::POST) {
        Ok(actor) => actor,
        Err(response) => return *response,
    };
    let Some(backups) = state.backups() else {
        return unavailable(&headers);
    };
    match backups.create(BackupSource::Manual, &actor).await {
        Ok(data) => created(&headers, data),
        Err(error) => backup_error(&headers, error),
    }
}

async fn upload_backup(
    State(state): State<WebApiState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let actor = match admin_actor(&state, &headers, &Method::POST) {
        Ok(actor) => actor,
        Err(response) => return *response,
    };
    let (Some(service), Some(backups)) = (state.administration(), state.backups()) else {
        return unavailable(&headers);
    };
    let maximum = match service.runtime_settings().await {
        Ok(settings) => settings
            .into_iter()
            .find(|setting| setting.key == "backups.upload_max_bytes")
            .and_then(|setting| u64::try_from(setting.value).ok()),
        Err(error) => return admin_error(&headers, error),
    };
    let Some(maximum) = maximum else {
        return admin_error(&headers, AdminError::Database);
    };
    let field = match multipart.next_field().await {
        Ok(Some(field))
            if field.name() == Some("file")
                && matches!(
                    field.content_type(),
                    Some("application/vnd.sqlite3" | "application/x-sqlite3")
                ) =>
        {
            field
        }
        _ => return backup_error(&headers, BackupError::Invalid),
    };
    match backups
        .upload(
            field.map(|chunk| chunk.map_err(|_| BackupError::Invalid)),
            maximum,
            &actor,
        )
        .await
    {
        Ok(data) => created(&headers, data),
        Err(error) => backup_error(&headers, error),
    }
}

async fn download_backup(
    State(state): State<WebApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = actor(&state, &headers) {
        return *response;
    }
    let Some(backups) = state.backups() else {
        return unavailable(&headers);
    };
    let (record, path) = match backups.download(&id).await {
        Ok(download) => download,
        Err(error) => return backup_error(&headers, error),
    };
    let file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(_) => return backup_error(&headers, BackupError::Io),
    };
    let disposition =
        match HeaderValue::from_str(&format!("attachment; filename=\"{}\"", record.file_name)) {
            Ok(value) => value,
            Err(_) => return backup_error(&headers, BackupError::Database),
        };
    let digest = match HeaderValue::from_str(&format!("sha-256={}", record.sha256)) {
        Ok(value) => value,
        Err(_) => return backup_error(&headers, BackupError::Database),
    };
    let mut response = Response::new(Body::from_stream(ReaderStream::new(file)));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/vnd.sqlite3"),
    );
    response
        .headers_mut()
        .insert(CONTENT_DISPOSITION, disposition);
    response.headers_mut().insert("digest", digest);
    response
}

async fn verify_backup(
    State(state): State<WebApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let actor = match admin_actor(&state, &headers, &Method::POST) {
        Ok(actor) => actor,
        Err(response) => return *response,
    };
    let Some(backups) = state.backups() else {
        return unavailable(&headers);
    };
    match backups.verify(&id, &actor).await {
        Ok(data) => success(&headers, data),
        Err(error) => backup_error(&headers, error),
    }
}

async fn restore_backup(
    State(state): State<WebApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<RestoreRequest>,
) -> Response {
    if state.sessions().mode() != WebMode::Authenticated {
        return reauthentication_required(&headers);
    }
    let actor = match admin_actor(&state, &headers, &Method::POST) {
        Ok(actor) => actor,
        Err(response) => return *response,
    };
    let now = OffsetDateTime::now_utc();
    let session = match state.sessions().authenticate(&headers, now) {
        Ok(session) => session,
        Err(_) => return reauthentication_required(&headers),
    };
    if !recently_reauthenticated(session.reauthenticated_at, now) {
        return reauthentication_required(&headers);
    }
    let Some(restore) = state.restore() else {
        return unavailable(&headers);
    };
    match restore
        .restore(&id, &request.confirmation, &actor, RestoreFault::None)
        .await
    {
        Ok(data) => success::<RestoreOutcome>(&headers, data),
        Err(error) => backup_error(&headers, error),
    }
}

fn reauthentication_required(headers: &HeaderMap) -> Response {
    api_error(
        headers,
        StatusCode::UNAUTHORIZED,
        "REAUTHENTICATION_REQUIRED",
        "Recent password verification is required.",
        false,
        None,
    )
}

fn created(headers: &HeaderMap, data: BackupRecord) -> Response {
    let mut response = success(headers, data);
    *response.status_mut() = StatusCode::CREATED;
    response
}

fn admin_actor(
    state: &WebApiState,
    headers: &HeaderMap,
    method: &Method,
) -> Result<AdminActor, Box<Response>> {
    let actor = authorize_mutation(state, headers, method)?;
    AdminActor::new(actor.actor_id, actor.request_id)
        .map_err(|error| Box::new(admin_error(headers, error)))
}

fn admin_error(headers: &HeaderMap, error: AdminError) -> Response {
    match error {
        AdminError::Validation => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            "VALIDATION_ERROR",
            "Administrative input failed validation.",
            false,
            None,
        ),
        AdminError::VersionConflict => api_error(
            headers,
            StatusCode::CONFLICT,
            "VERSION_CONFLICT",
            "The expected administrative version is stale.",
            true,
            None,
        ),
        AdminError::Database => api_error(
            headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            "The administrative operation failed.",
            false,
            None,
        ),
    }
}

fn backup_error(headers: &HeaderMap, error: BackupError) -> Response {
    match error {
        BackupError::Invalid => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            "BACKUP_INVALID",
            "The backup failed verification.",
            false,
            None,
        ),
        BackupError::LimitExceeded => api_error(
            headers,
            StatusCode::PAYLOAD_TOO_LARGE,
            "LIMIT_EXCEEDED",
            "The backup upload exceeded its configured limit.",
            false,
            None,
        ),
        BackupError::NotFound => api_error(
            headers,
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "The requested backup was not found.",
            false,
            None,
        ),
        BackupError::RestoreConfirmation => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            "VALIDATION_ERROR",
            "The exact restore confirmation phrase is required.",
            false,
            None,
        ),
        BackupError::RestoreFailed | BackupError::Workload => api_error(
            headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            "RESTORE_FAILED",
            "Restore failed and rollback was attempted.",
            false,
            None,
        ),
        BackupError::Database | BackupError::Io => api_error(
            headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            "The administrative operation failed.",
            false,
            None,
        ),
    }
}

fn unavailable(headers: &HeaderMap) -> Response {
    api_error(
        headers,
        StatusCode::NOT_FOUND,
        "NOT_FOUND",
        "The requested resource was not found.",
        false,
        None,
    )
}

fn recently_reauthenticated(reauthenticated_at: OffsetDateTime, now: OffsetDateTime) -> bool {
    reauthenticated_at <= now && now - reauthenticated_at <= Duration::minutes(5)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn restore_reauthentication_window_is_exactly_five_minutes_and_not_future_dated() {
        let now = datetime!(2026-07-19 03:00 UTC);
        assert!(recently_reauthenticated(now - Duration::minutes(5), now));
        assert!(!recently_reauthenticated(
            now - Duration::minutes(5) - Duration::nanoseconds(1),
            now
        ));
        assert!(!recently_reauthenticated(
            now + Duration::nanoseconds(1),
            now
        ));
    }
}
