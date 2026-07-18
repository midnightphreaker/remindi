use std::sync::{Arc, RwLock};

use axum::{
    body::Body,
    extract::Request,
    http::{StatusCode, header::RETRY_AFTER},
    response::{IntoResponse, Response},
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use secrecy::ExposeSecret;
use serde_json::json;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

use crate::{
    app::AppState,
    auth::mcp::{McpAuthError, McpAuthPolicy, McpIdentity},
    clock::IdGenerator,
    remindi::{Actor, RemindiService},
};

use super::McpServer;

const RETRY_AFTER_SECONDS: &str = "1";

type HttpService = StreamableHttpService<McpServer, LocalSessionManager>;

struct RunningMcp {
    service: HttpService,
    cancellation: CancellationToken,
}

/// In-process lifecycle owner for the rmcp Streamable HTTP workload.
pub struct McpWorkload {
    auth: McpAuthPolicy,
    identity: Arc<RwLock<Option<McpIdentity>>>,
    service: Arc<RemindiService>,
    ids: Arc<dyn IdGenerator>,
    running: RwLock<Option<RunningMcp>>,
}

/// Safe MCP workload assembly or lifecycle error.
#[derive(Clone, Copy, Debug, Error)]
pub enum McpWorkloadError {
    #[error("the MCP workload requires an initialized database")]
    DatabaseUnavailable,
    #[error("the MCP request policy is invalid")]
    InvalidPolicy,
    #[error("the MCP workload state is unavailable")]
    StateUnavailable,
}

impl McpWorkload {
    /// Builds and starts the MCP workload from process-owned state.
    ///
    /// # Errors
    ///
    /// Returns a safe error when the database is absent or request policy is invalid.
    pub fn new(state: &AppState) -> Result<Self, McpWorkloadError> {
        let bootstrap = state.bootstrap();
        let database = state
            .database_shared()
            .ok_or(McpWorkloadError::DatabaseUnavailable)?;
        let auth = McpAuthPolicy::new(
            bootstrap.owner_id(),
            bootstrap.mcp_token(),
            bootstrap.allowed_hosts().iter().map(String::as_str),
            bootstrap.allowed_origins().iter().map(String::as_str),
        )
        .map_err(|_| McpWorkloadError::InvalidPolicy)?;
        let service = Arc::new(RemindiService::new(
            database,
            bootstrap.owner_id(),
            bootstrap.mcp_token().expose_secret().as_bytes(),
            state.clock_shared(),
            state.ids_shared(),
        ));
        let workload = Self {
            auth,
            identity: Arc::new(RwLock::new(None)),
            service,
            ids: state.ids_shared(),
            running: RwLock::new(None),
        };
        workload.start()?;
        Ok(workload)
    }

    /// Reports whether the workload currently accepts MCP traffic.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running.read().is_ok_and(|running| running.is_some())
    }

    /// Starts a fresh rmcp service and in-memory transport-session manager.
    ///
    /// # Errors
    ///
    /// Returns an error if the lifecycle lock is unavailable.
    pub fn start(&self) -> Result<(), McpWorkloadError> {
        let mut running = self
            .running
            .write()
            .map_err(|_| McpWorkloadError::StateUnavailable)?;
        if running.is_some() {
            return Ok(());
        }
        let cancellation = CancellationToken::new();
        let identity = Arc::clone(&self.identity);
        let remindi = Arc::clone(&self.service);
        let ids = Arc::clone(&self.ids);
        let service = StreamableHttpService::new(
            move || {
                let identity = Arc::clone(&identity);
                let ids = Arc::clone(&ids);
                Ok(McpServer::new(Arc::clone(&remindi), move || {
                    let actor_id = identity
                        .read()
                        .ok()
                        .and_then(|identity| identity.as_ref().cloned())
                        .map_or_else(
                            || "mcp:unavailable".to_owned(),
                            |identity| identity.actor_id().to_owned(),
                        );
                    Actor::agent(actor_id, Some(format!("req_{}", ids.next_id().simple())))
                }))
            },
            Arc::new(LocalSessionManager::default()),
            StreamableHttpServerConfig::default()
                .disable_allowed_hosts()
                .disable_allowed_origins()
                .with_sse_keep_alive(None)
                .with_cancellation_token(cancellation.child_token()),
        );
        *running = Some(RunningMcp {
            service,
            cancellation,
        });
        Ok(())
    }

    /// Stops new MCP traffic and invalidates every in-memory transport session.
    ///
    /// # Errors
    ///
    /// Returns an error if the lifecycle lock is unavailable.
    pub fn stop(&self) -> Result<(), McpWorkloadError> {
        let running = self
            .running
            .write()
            .map_err(|_| McpWorkloadError::StateUnavailable)?
            .take();
        if let Some(running) = running {
            running.cancellation.cancel();
        }
        Ok(())
    }

    /// Replaces the rmcp workload with a fresh session manager.
    ///
    /// # Errors
    ///
    /// Returns an error if stop or start cannot acquire lifecycle state.
    pub fn restart(&self) -> Result<(), McpWorkloadError> {
        self.stop()?;
        self.start()
    }

    /// Authenticates and delegates one request, or returns a bounded control response.
    pub async fn handle(&self, request: Request) -> Response {
        let identity = match self.auth.authenticate(request.headers(), request.uri()) {
            Ok(identity) => identity,
            Err(error) => return auth_failure(error),
        };
        if let Ok(mut current) = self.identity.write() {
            *current = Some(identity);
        } else {
            return safe_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "MCP workload is unavailable",
                None,
            );
        }
        let service = match self.running.read() {
            Ok(running) => running.as_ref().map(|running| running.service.clone()),
            Err(_) => None,
        };
        let Some(service) = service else {
            return safe_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "MCP workload is stopped",
                Some(RETRY_AFTER_SECONDS),
            );
        };
        match service.oneshot(request).await {
            Ok(response) => response.map(Body::new),
            Err(error) => match error {},
        }
    }
}

fn auth_failure(error: McpAuthError) -> Response {
    match error {
        McpAuthError::Unauthenticated => {
            safe_response(StatusCode::UNAUTHORIZED, "MCP authentication failed", None)
        }
        McpAuthError::OriginRejected => {
            safe_response(StatusCode::FORBIDDEN, "MCP origin rejected", None)
        }
        McpAuthError::CredentialLocation | McpAuthError::HostRejected => {
            safe_response(StatusCode::BAD_REQUEST, "MCP request rejected", None)
        }
        McpAuthError::InvalidHostPolicy | McpAuthError::InvalidOriginPolicy => safe_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "MCP workload is unavailable",
            None,
        ),
    }
}

fn safe_response(status: StatusCode, message: &'static str, retry_after: Option<&str>) -> Response {
    let mut response = (status, axum::Json(json!({"error": message}))).into_response();
    if let Some(seconds) = retry_after {
        response.headers_mut().insert(
            RETRY_AFTER,
            seconds
                .parse()
                .expect("static Retry-After value is a valid header"),
        );
    }
    response
}
