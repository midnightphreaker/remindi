use std::sync::Arc;

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::{
    admin::{
        AdminService,
        backup::{BackupManager, RestoreManager},
        workloads::WorkloadController,
    },
    auth::{
        csrf::{self, CSRF_HEADER},
        web_session::{LoginError, SessionError, WebMode, WebSessionManager},
    },
    remindi::{Actor, ActorType, RemindiService, ServiceError},
};

pub mod admin;
pub mod remindi;

const JSON_BODY_LIMIT: usize = 1024 * 1024;

#[derive(Clone)]
pub struct WebApiState {
    sessions: WebSessionManager,
    service: Arc<RemindiService>,
    administration: Option<Arc<AdminService>>,
    workloads: Option<Arc<WorkloadController>>,
    backups: Option<Arc<BackupManager>>,
    restore: Option<Arc<RestoreManager>>,
}

impl WebApiState {
    #[must_use]
    pub fn new(sessions: WebSessionManager, service: Arc<RemindiService>) -> Self {
        Self {
            sessions,
            service,
            administration: None,
            workloads: None,
            backups: None,
            restore: None,
        }
    }

    /// Attaches the authenticated administration and in-process lifecycle seams.
    #[must_use]
    pub fn with_administration(
        mut self,
        administration: Arc<AdminService>,
        workloads: Arc<WorkloadController>,
    ) -> Self {
        self.administration = Some(administration);
        self.workloads = Some(workloads);
        self
    }

    /// Attaches verified backup administration to the authenticated API.
    #[must_use]
    pub fn with_backups(mut self, backups: Arc<BackupManager>) -> Self {
        self.backups = Some(backups);
        self
    }

    /// Attaches guarded restore administration to the authenticated API.
    #[must_use]
    pub fn with_restore(mut self, restore: Arc<RestoreManager>) -> Self {
        self.restore = Some(restore);
        self
    }

    #[must_use]
    pub fn sessions(&self) -> &WebSessionManager {
        &self.sessions
    }

    #[must_use]
    pub fn service(&self) -> &RemindiService {
        &self.service
    }

    pub(crate) fn administration(&self) -> Option<&AdminService> {
        self.administration.as_deref()
    }

    pub(crate) fn workloads(&self) -> Option<&WorkloadController> {
        self.workloads.as_deref()
    }

    pub(crate) fn backups(&self) -> Option<&BackupManager> {
        self.backups.as_deref()
    }

    pub(crate) fn restore(&self) -> Option<&RestoreManager> {
        self.restore.as_deref()
    }
}

/// Builds the complete Task 9 route subtree, ready to nest at `/api/v1`.
pub fn router(state: WebApiState) -> Router {
    if state.sessions.mode() == WebMode::Disabled {
        return Router::new();
    }
    let administration = state.administration.is_some() && state.workloads.is_some();
    let mut router = Router::new()
        .route("/session", get(session))
        .route("/auth/login", post(login))
        .route("/auth/reauthenticate", post(reauthenticate))
        .route("/auth/logout", post(logout));
    if administration {
        router = router.merge(admin::routes());
    }
    router
        .merge(remindi::router())
        .layer(DefaultBodyLimit::max(JSON_BODY_LIMIT))
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReauthenticateRequest {
    password: String,
}

#[derive(Serialize)]
struct SessionData {
    authenticated: bool,
    authentication_required: bool,
    actor_id: Option<String>,
    csrf_token: String,
    expires_at: Option<String>,
    reauthentication_required: bool,
}

async fn session(State(state): State<WebApiState>, headers: HeaderMap) -> Response {
    let now = OffsetDateTime::now_utc();
    match state.sessions.authenticate(&headers, now) {
        Ok(session) => success(
            &headers,
            SessionData {
                authenticated: true,
                authentication_required: state.sessions.mode() == WebMode::Authenticated,
                actor_id: Some(session.actor_id),
                csrf_token: session.csrf_token,
                expires_at: (state.sessions.mode() == WebMode::Authenticated)
                    .then(|| format_time(session.expires_at)),
                reauthentication_required: false,
            },
        ),
        Err(SessionError::Unauthenticated | SessionError::Expired) => {
            match state.sessions.issue_pre_session_token(now) {
                Ok(token) => success(
                    &headers,
                    SessionData {
                        authenticated: false,
                        authentication_required: true,
                        actor_id: None,
                        csrf_token: token,
                        expires_at: None,
                        reauthentication_required: false,
                    },
                ),
                Err(_) => api_error(
                    &headers,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "INTERNAL_ERROR",
                    "The request could not be completed.",
                    false,
                    None,
                ),
            }
        }
        Err(SessionError::Disabled) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "The requested resource was not found.",
            false,
            None,
        ),
    }
}

async fn login(
    State(state): State<WebApiState>,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Response {
    if csrf::validate_same_origin(&headers).is_err() {
        return csrf_error(&headers);
    }
    let token = match (
        headers.get_all(CSRF_HEADER).iter().count(),
        headers
            .get(CSRF_HEADER)
            .and_then(|value| value.to_str().ok()),
    ) {
        (1, Some(token)) => token,
        _ => return csrf_error(&headers),
    };
    match state.sessions.login(
        &body.username,
        &body.password,
        token,
        OffsetDateTime::now_utc(),
    ) {
        Ok(result) => {
            let mut response = success(
                &headers,
                SessionData {
                    authenticated: true,
                    authentication_required: true,
                    actor_id: Some(result.session.actor_id),
                    csrf_token: result.session.csrf_token,
                    expires_at: Some(format_time(result.session.expires_at)),
                    reauthentication_required: false,
                },
            );
            response
                .headers_mut()
                .insert(header::SET_COOKIE, result.set_cookie);
            response
        }
        Err(LoginError::CsrfRejected) => csrf_error(&headers),
        Err(LoginError::RateLimited) => api_error(
            &headers,
            StatusCode::TOO_MANY_REQUESTS,
            "LIMIT_EXCEEDED",
            "Sign-in is temporarily unavailable.",
            false,
            None,
        ),
        Err(LoginError::InvalidCredentials | LoginError::Disabled) => api_error(
            &headers,
            StatusCode::UNAUTHORIZED,
            "UNAUTHENTICATED",
            "Sign-in failed.",
            false,
            None,
        ),
        Err(LoginError::Randomness) => api_error(
            &headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            "The request could not be completed.",
            false,
            None,
        ),
    }
}

async fn logout(State(state): State<WebApiState>, headers: HeaderMap) -> Response {
    let session = match authorize_mutation(&state, &headers, &http::Method::POST) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let _ = session;
    let mut response = success(&headers, json!({"logged_out": true}));
    response
        .headers_mut()
        .insert(header::SET_COOKIE, state.sessions.logout(&headers));
    response
}

async fn reauthenticate(
    State(state): State<WebApiState>,
    headers: HeaderMap,
    Json(body): Json<ReauthenticateRequest>,
) -> Response {
    if let Err(response) = authorize_mutation(&state, &headers, &http::Method::POST) {
        return *response;
    }
    match state
        .sessions
        .reauthenticate(&headers, &body.password, OffsetDateTime::now_utc())
    {
        Ok(session) => success(
            &headers,
            SessionData {
                authenticated: true,
                authentication_required: true,
                actor_id: Some(session.actor_id),
                csrf_token: session.csrf_token,
                expires_at: Some(format_time(session.expires_at)),
                reauthentication_required: false,
            },
        ),
        Err(LoginError::RateLimited) => api_error(
            &headers,
            StatusCode::TOO_MANY_REQUESTS,
            "LIMIT_EXCEEDED",
            "Password verification is temporarily unavailable.",
            false,
            None,
        ),
        Err(
            LoginError::InvalidCredentials
            | LoginError::Disabled
            | LoginError::CsrfRejected
            | LoginError::Randomness,
        ) => api_error(
            &headers,
            StatusCode::UNAUTHORIZED,
            "REAUTHENTICATION_REQUIRED",
            "Recent password verification is required.",
            false,
            None,
        ),
    }
}

pub(crate) fn actor(state: &WebApiState, headers: &HeaderMap) -> Result<Actor, Box<Response>> {
    let session = state
        .sessions
        .authenticate(headers, OffsetDateTime::now_utc())
        .map_err(|_| Box::new(unauthenticated(headers)))?;
    Ok(Actor {
        actor_type: ActorType::User,
        actor_id: session.actor_id,
        request_id: Some(request_id(headers)),
    })
}

pub(crate) fn authorize_mutation(
    state: &WebApiState,
    headers: &HeaderMap,
    method: &http::Method,
) -> Result<Actor, Box<Response>> {
    let session = state
        .sessions
        .authenticate(headers, OffsetDateTime::now_utc())
        .map_err(|_| Box::new(unauthenticated(headers)))?;
    csrf::validate_mutation(method, headers, &session.csrf_token)
        .map_err(|_| Box::new(csrf_error(headers)))?;
    Ok(Actor {
        actor_type: ActorType::User,
        actor_id: session.actor_id,
        request_id: Some(request_id(headers)),
    })
}

pub(crate) fn success<T: Serialize>(headers: &HeaderMap, data: T) -> Response {
    (
        StatusCode::OK,
        Json(json!({"ok": true, "request_id": request_id(headers), "data": data})),
    )
        .into_response()
}

pub(crate) fn service_error(headers: &HeaderMap, error: ServiceError) -> Response {
    match error {
        ServiceError::Validation | ServiceError::InvalidCursor => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            "VALIDATION_ERROR",
            "Input failed validation.",
            false,
            None,
        ),
        ServiceError::NotFound => api_error(
            headers,
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "The Remindi item was not found.",
            false,
            None,
        ),
        ServiceError::InvalidState => api_error(
            headers,
            StatusCode::CONFLICT,
            "INVALID_STATE",
            "The operation is not allowed in the current state.",
            false,
            None,
        ),
        ServiceError::VersionConflict { current_version } => api_error(
            headers,
            StatusCode::CONFLICT,
            "VERSION_CONFLICT",
            "The Remindi item changed since it was read.",
            true,
            Some(json!({"current_version": current_version})),
        ),
        ServiceError::IdempotencyKeyReused => api_error(
            headers,
            StatusCode::CONFLICT,
            "IDEMPOTENCY_KEY_REUSED",
            "The idempotency key was reused with different input.",
            false,
            None,
        ),
        ServiceError::DatabaseBusy => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            "DATABASE_BUSY",
            "The database is busy; retry the request.",
            true,
            None,
        ),
        ServiceError::MaintenanceActive => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            "MAINTENANCE_ACTIVE",
            "Database maintenance is active; retry the request.",
            true,
            None,
        ),
        ServiceError::Internal => api_error(
            headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            "The request could not be completed.",
            false,
            None,
        ),
    }
}

fn unauthenticated(headers: &HeaderMap) -> Response {
    api_error(
        headers,
        StatusCode::UNAUTHORIZED,
        "UNAUTHENTICATED",
        "Authentication is required.",
        false,
        None,
    )
}

fn csrf_error(headers: &HeaderMap) -> Response {
    api_error(
        headers,
        StatusCode::FORBIDDEN,
        "CSRF_REJECTED",
        "The browser request was rejected.",
        false,
        None,
    )
}

pub(crate) fn api_error(
    headers: &HeaderMap,
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    retryable: bool,
    details: Option<Value>,
) -> Response {
    (
        status,
        Json(json!({
            "ok": false,
            "request_id": request_id(headers),
            "error": {
                "code": code,
                "message": message,
                "retryable": retryable,
                "details": details.unwrap_or_else(|| json!({}))
            }
        })),
    )
        .into_response()
}

fn request_id(headers: &HeaderMap) -> String {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty() && value.len() <= 128)
        .unwrap_or("request-unavailable")
        .to_owned()
}

fn format_time(value: OffsetDateTime) -> String {
    crate::remindi::canonical_timestamp(value).unwrap_or_else(|_| "invalid".to_owned())
}

/// Adds the restrictive browser response policy to a WebUI or API subtree.
///
/// Task 10 can apply this same middleware to the embedded `/` and `/assets/*`
/// routes while this module applies it to the JSON API.
pub async fn security_headers(request: Request<Body>, next: Next) -> Response {
    let request_headers = request.headers().clone();
    let mut response = next.run(request).await;
    let status = response.status();
    let is_json = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"));
    if !is_json
        && matches!(
            status,
            StatusCode::BAD_REQUEST
                | StatusCode::PAYLOAD_TOO_LARGE
                | StatusCode::UNSUPPORTED_MEDIA_TYPE
                | StatusCode::UNPROCESSABLE_ENTITY
        )
    {
        response = if status == StatusCode::PAYLOAD_TOO_LARGE {
            api_error(
                &request_headers,
                status,
                "LIMIT_EXCEEDED",
                "The request body exceeded the configured limit.",
                false,
                None,
            )
        } else {
            api_error(
                &request_headers,
                status,
                "VALIDATION_ERROR",
                "Input failed validation.",
                false,
                None,
            )
        };
    }
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
        ),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
