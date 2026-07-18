use std::{str::FromStr, sync::Arc, time::Duration};

use axum::{
    body::{Body, Bytes},
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use remindi::{
    app::{AppState, run},
    clock::{Clock, FixedClock, FixedIdGenerator, IdGenerator},
    config::{BootstrapConfig, LISTEN_ADDRESS},
    http::router::build_router,
};
use rmcp::{
    ServerHandler,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use schemars::{JsonSchema, schema_for};
use secrecy::ExposeSecret;
use serde::Serialize;
use sqlx::sqlite::SqliteConnectOptions;
use time::{OffsetDateTime, macros::datetime};
use tower::ServiceExt;
use uuid::Uuid;

const MCP_TOKEN: &str = "task-one-secret-token-material";
const WEBUI_PASSWORD: &str = "task-one-webui-password";
const FIXED_ID: &str = "0198c3b4-8580-7000-8000-000000000001";

fn required_environment() -> Vec<(&'static str, &'static str)> {
    vec![
        ("REMINDI_OWNER_ID", "owner-1"),
        ("REMINDI_MCP_TOKEN", MCP_TOKEN),
        ("REMINDI_WEBUI_USERNAME", "owner"),
        ("REMINDI_WEBUI_PASSWORD", WEBUI_PASSWORD),
    ]
}

fn test_state() -> AppState {
    let config = BootstrapConfig::from_pairs(required_environment()).expect("valid test config");
    let now = datetime!(2026-07-18 12:00 UTC);
    let id = Uuid::from_str(FIXED_ID).expect("fixed UUID is valid");

    AppState::new(
        Arc::new(config),
        Arc::new(FixedClock::new(now)),
        Arc::new(FixedIdGenerator::new(id)),
    )
}

async fn response_body(response: axum::response::Response) -> Bytes {
    response
        .into_body()
        .collect()
        .await
        .expect("response body is readable")
        .to_bytes()
}

#[test]
fn bootstrap_config_uses_the_documented_defaults() {
    let config = BootstrapConfig::from_pairs(required_environment()).expect("valid config");

    assert_eq!(config.database_path().to_str(), Some("/data/remindi.db"));
    assert_eq!(config.backup_directory().to_str(), Some("/data/backups"));
    assert_eq!(config.owner_id(), "owner-1");
    assert_eq!(config.allowed_hosts(), &[] as &[String]);
    assert_eq!(config.allowed_origins(), &[] as &[String]);
    assert_eq!(config.log_level(), "info");
    assert!(!config.log_content());
    assert!(config.webui_enabled());
    assert!(config.webui_auth_enabled());
    assert_eq!(config.webui_session_ttl_seconds(), 43_200);
    assert!(!config.webui_cookie_secure());
    assert_eq!(config.webui_title(), "Remindi");
    assert_eq!(config.listener_address(), LISTEN_ADDRESS);
}

#[test]
fn bootstrap_config_retains_credentials_in_secret_wrappers() {
    let config = BootstrapConfig::from_pairs(required_environment()).expect("valid config");

    assert_eq!(config.mcp_token().expose_secret(), MCP_TOKEN);
    assert_eq!(
        config
            .webui_password()
            .expect("password is configured")
            .expose_secret(),
        WEBUI_PASSWORD
    );
    assert!(!format!("{:?}", config.mcp_token()).contains(MCP_TOKEN));
    assert!(!format!("{:?}", config.webui_password()).contains(WEBUI_PASSWORD));
}

#[test]
fn bootstrap_config_rejects_missing_required_values_without_echoing_secrets() {
    let error = match BootstrapConfig::from_pairs([
        ("REMINDI_OWNER_ID", "owner-1"),
        ("REMINDI_MCP_TOKEN", MCP_TOKEN),
        ("REMINDI_WEBUI_USERNAME", "owner"),
        ("REMINDI_WEBUI_PASSWORD", ""),
    ]) {
        Ok(_) => panic!("blank WebUI password must fail"),
        Err(error) => error,
    };

    assert_eq!(
        error.to_string(),
        "REMINDI_WEBUI_PASSWORD is required when WebUI authentication is enabled"
    );
    assert!(!error.to_string().contains(MCP_TOKEN));
}

#[test]
fn bootstrap_config_errors_do_not_echo_invalid_values() {
    let invalid_secret_value = "not-a-bool-secret-value";
    let mut environment = required_environment();
    environment.push(("REMINDI_LOG_CONTENT", invalid_secret_value));

    let error = match BootstrapConfig::from_pairs(environment) {
        Ok(_) => panic!("invalid boolean must fail"),
        Err(error) => error,
    };

    assert_eq!(
        error.to_string(),
        "REMINDI_LOG_CONTENT must be `true` or `false`"
    );
    assert!(!error.to_string().contains(invalid_secret_value));
}

#[test]
fn fixed_clock_and_id_generator_are_deterministic() {
    let expected_time: OffsetDateTime = datetime!(2026-07-18 12:00 UTC);
    let expected_id = Uuid::from_str(FIXED_ID).expect("fixed UUID is valid");
    let clock = FixedClock::new(expected_time);
    let ids = FixedIdGenerator::new(expected_id);

    assert_eq!(clock.now(), expected_time);
    assert_eq!(ids.next_id(), expected_id);
    assert_eq!(ids.next_id(), expected_id);
}

#[derive(Clone)]
struct CompatibilityMcp;

impl ServerHandler for CompatibilityMcp {}

#[derive(JsonSchema, Serialize)]
struct CompatibilitySchema {
    id: Uuid,
}

#[test]
fn selected_rmcp_axum_schemars_and_sqlx_apis_compile_together() {
    let _schema = schema_for!(CompatibilitySchema);
    let _sqlite = SqliteConnectOptions::from_str("sqlite::memory:")
        .expect("in-memory SQLite options compile");
    let service: StreamableHttpService<CompatibilityMcp, LocalSessionManager> =
        StreamableHttpService::new(
            || Ok(CompatibilityMcp),
            Default::default(),
            StreamableHttpServerConfig::default(),
        );
    let _router: axum::Router = axum::Router::new().nest_service("/mcp", service);
}

#[tokio::test]
async fn liveness_is_minimal_and_carries_a_deterministic_request_id() {
    let response = build_router(test_state())
        .oneshot(
            Request::builder()
                .uri("/health/live")
                .body(Body::empty())
                .expect("request is valid"),
        )
        .await
        .expect("health request succeeds");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-request-id"],
        format!("req_{}", FIXED_ID.replace('-', ""))
    );
    assert_eq!(
        response_body(response).await,
        Bytes::from_static(br#"{"status":"ok"}"#)
    );
}

#[tokio::test]
async fn readiness_fails_closed_until_the_application_is_ready() {
    let state = test_state();
    let response = build_router(state)
        .oneshot(
            Request::builder()
                .uri("/health/ready")
                .body(Body::empty())
                .expect("request is valid"),
        )
        .await
        .expect("readiness request succeeds");

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response_body(response).await,
        Bytes::from_static(br#"{"status":"starting"}"#)
    );
}

#[tokio::test]
async fn api_not_found_uses_the_structured_error_envelope_and_request_id() {
    let response = build_router(test_state())
        .oneshot(
            Request::builder()
                .uri("/api/v1/not-present")
                .body(Body::empty())
                .expect("request is valid"),
        )
        .await
        .expect("request succeeds");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response_body(response).await,
        Bytes::from_static(
            br#"{"ok":false,"request_id":"req_0198c3b4858070008000000000000001","error":{"code":"NOT_FOUND","message":"The requested resource was not found.","retryable":false,"details":{}}}"#,
        )
    );
}

#[tokio::test]
async fn server_stops_cleanly_when_the_shutdown_future_resolves() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("ephemeral listener binds");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(run(listener, build_router(test_state()), async move {
        let _ = shutdown_rx.await;
    }));

    shutdown_tx
        .send(())
        .expect("shutdown receiver remains active");
    let result = tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("server stops before the deadline")
        .expect("server task joins");

    assert!(result.is_ok(), "graceful server error: {result:?}");
}
