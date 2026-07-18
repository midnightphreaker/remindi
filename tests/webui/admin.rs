use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Request, Response, StatusCode, header},
};
use http_body_util::BodyExt;
use remindi::{
    admin::{
        AdminService,
        workloads::{WorkloadController, WorkloadRuntime},
    },
    auth::web_session::WebSessionManager,
    clock::{FixedClock, IdGenerator},
    config::BootstrapConfig,
    db::DatabaseManager,
    http::api::{WebApiState, router},
    remindi::RemindiService,
    scheduler::AdapterProvider,
};
use serde_json::{Value, json};
use time::macros::datetime;
use tokio::sync::Notify;
use tower::ServiceExt;
use uuid::Uuid;

#[derive(Default)]
struct SequenceIds(AtomicU64);

impl IdGenerator for SequenceIds {
    fn next_id(&self) -> Uuid {
        Uuid::from_u128(u128::from(self.0.fetch_add(1, Ordering::Relaxed) + 1))
    }
}

struct ProbeRuntime(AtomicBool);

#[async_trait]
impl WorkloadRuntime for ProbeRuntime {
    fn is_running(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    async fn start(&self) -> Result<(), String> {
        self.0.store(true, Ordering::Release);
        Ok(())
    }

    async fn stop(&self) -> Result<(), String> {
        self.0.store(false, Ordering::Release);
        Ok(())
    }
}

struct BlockingRuntime {
    running: AtomicBool,
    stop_entered: Notify,
    release_stop: Notify,
}

#[async_trait]
impl WorkloadRuntime for BlockingRuntime {
    fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    async fn start(&self) -> Result<(), String> {
        self.running.store(true, Ordering::Release);
        Ok(())
    }

    async fn stop(&self) -> Result<(), String> {
        self.stop_entered.notify_one();
        self.release_stop.notified().await;
        self.running.store(false, Ordering::Release);
        Ok(())
    }
}

struct Fixture {
    app: Router,
    database: Arc<DatabaseManager>,
    admin: Arc<AdminService>,
}

async fn fixture() -> Fixture {
    fixture_with_runtimes(
        Arc::new(ProbeRuntime(AtomicBool::new(false))),
        Arc::new(ProbeRuntime(AtomicBool::new(false))),
    )
    .await
}

async fn fixture_with_runtimes(
    mcp: Arc<dyn WorkloadRuntime>,
    scheduler: Arc<dyn WorkloadRuntime>,
) -> Fixture {
    let directory = std::env::temp_dir().join(format!("remindi-web-admin-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&directory).expect("temporary directory");
    let config = Arc::new(
        BootstrapConfig::from_pairs([
            (
                "REMINDI_DB_PATH",
                directory.join("remindi.db").to_str().expect("UTF-8 path"),
            ),
            ("REMINDI_OWNER_ID", "owner-private"),
            ("REMINDI_MCP_TOKEN", "mcp-private-token"),
            ("REMINDI_WEBUI_AUTH", "false"),
            ("REMINDI_WEBUI_USERNAME", "username-private"),
            ("REMINDI_WEBUI_PASSWORD", "password-private"),
        ])
        .expect("config"),
    );
    let database = Arc::new(
        DatabaseManager::open(config.database_path())
            .await
            .expect("database"),
    );
    let clock = Arc::new(FixedClock::new(datetime!(2026-07-19 06:00 UTC)));
    let ids: Arc<dyn IdGenerator> = Arc::new(SequenceIds::default());
    let remindi = Arc::new(RemindiService::new(
        Arc::clone(&database),
        config.owner_id(),
        b"cursor-key",
        clock.clone(),
        Arc::clone(&ids),
    ));
    let admin = Arc::new(
        AdminService::load(
            Arc::clone(&database),
            Arc::clone(&config),
            clock.clone(),
            Arc::clone(&ids),
        )
        .await
        .expect("admin service"),
    );
    let workloads = Arc::new(
        WorkloadController::from_runtimes(Arc::clone(&database), clock, mcp, scheduler)
            .await
            .expect("workloads"),
    );
    let state = WebApiState::new(
        WebSessionManager::from_config(&config).expect("sessions"),
        remindi,
    )
    .with_administration(Arc::clone(&admin), workloads);
    Fixture {
        app: router(state),
        database,
        admin,
    }
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    csrf: Option<&str>,
    body: Option<Value>,
) -> (Response<Body>, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "remindi.local")
        .header(header::ORIGIN, "http://remindi.local")
        .header("x-request-id", "request-admin-test");
    if let Some(csrf) = csrf {
        request = request.header("x-csrf-token", csrf);
    }
    let body = if let Some(value) = body {
        request = request.header(header::CONTENT_TYPE, "application/json");
        Body::from(value.to_string())
    } else {
        Body::empty()
    };
    let response = app
        .clone()
        .oneshot(request.body(body).expect("request"))
        .await
        .expect("response");
    let (parts, body) = response.into_parts();
    let bytes = body.collect().await.expect("body").to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (Response::from_parts(parts, Body::empty()), value)
}

async fn csrf(app: &Router) -> String {
    let (_, body) = call(app, "GET", "/session", None, None).await;
    body["data"]["csrf_token"]
        .as_str()
        .expect("CSRF token")
        .to_owned()
}

#[tokio::test]
async fn settings_are_redacted_versioned_and_every_attempt_is_audited() {
    let fixture = fixture().await;
    let csrf = csrf(&fixture.app).await;

    let (response, bootstrap) = call(&fixture.app, "GET", "/settings/bootstrap", None, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let encoded = bootstrap.to_string();
    for private in [
        "owner-private",
        "mcp-private-token",
        "username-private",
        "password-private",
    ] {
        assert!(!encoded.contains(private));
    }

    let (response, rejected) = call(
        &fixture.app,
        "PATCH",
        "/settings/adapters.timeout_seconds",
        None,
        Some(json!({"value": 6, "expected_version": 1})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(rejected["error"]["code"], "CSRF_REJECTED");

    let (response, updated) = call(
        &fixture.app,
        "PATCH",
        "/settings/adapters.timeout_seconds",
        Some(&csrf),
        Some(json!({"value": 6, "expected_version": 1})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(updated["data"]["version"], 2);

    let (response, conflict) = call(
        &fixture.app,
        "PATCH",
        "/settings/adapters.timeout_seconds",
        Some(&csrf),
        Some(json!({"value": 7, "expected_version": 1})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(conflict["error"]["code"], "VERSION_CONFLICT");

    let (_, audit) = call(&fixture.app, "GET", "/admin-events", None, None).await;
    let events = audit["data"].as_array().expect("events");
    assert_eq!(
        events.len(),
        2,
        "CSRF rejection occurs before admin mutation"
    );
    assert_eq!(events[0]["outcome"], "succeeded");
    assert_eq!(events[1]["outcome"], "rejected");
    assert_eq!(
        events[0]["details"],
        json!({"setting_name":"adapters.timeout_seconds"})
    );
    assert!(!audit.to_string().contains("mcp-private-token"));
}

#[tokio::test]
async fn typed_adapter_updates_publish_atomically_and_conflicts_are_redacted() {
    let fixture = fixture().await;
    let csrf = csrf(&fixture.app).await;
    let before = fixture
        .admin
        .adapters()
        .get("http_health")
        .expect("published adapter");
    let body = json!({
        "enabled": true,
        "expected_version": 1,
        "configuration": {
            "type": "http_health",
            "aliases": {
                "home": {
                    "url": "https://example.com/health",
                    "expected_statuses": [200],
                    "max_response_bytes": 1024,
                    "expected_content_type": "application/json",
                    "allow_redirects": false,
                    "allow_private": false
                }
            }
        }
    });
    let (response, updated) = call(
        &fixture.app,
        "PATCH",
        "/adapters/http_health",
        Some(&csrf),
        Some(body.clone()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(updated["data"]["version"], 2);
    let after = fixture
        .admin
        .adapters()
        .get("http_health")
        .expect("published adapter");
    assert!(!Arc::ptr_eq(&before, &after));

    let (response, conflict) = call(
        &fixture.app,
        "PATCH",
        "/adapters/http_health",
        Some(&csrf),
        Some(body),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(conflict["error"]["code"], "VERSION_CONFLICT");

    let mut connection = fixture.database.connection().await.expect("connection");
    let details: Vec<String> = sqlx::query_scalar(
        "SELECT details_json FROM admin_events \
         WHERE event_type = 'adapter_config_updated' ORDER BY sequence",
    )
    .fetch_all(connection.as_mut())
    .await
    .expect("audit");
    assert_eq!(
        details,
        [
            r#"{"adapter_name":"http_health"}"#,
            r#"{"adapter_name":"http_health","failure_code":"VERSION_CONFLICT"}"#
        ]
    );
    assert!(!details.join("").contains("example.com"));
}

#[tokio::test]
async fn workload_routes_keep_the_control_plane_available_and_audit_actions() {
    let fixture = fixture().await;
    let csrf = csrf(&fixture.app).await;

    let (response, initial) = call(&fixture.app, "GET", "/workloads", None, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        initial["data"]
            .as_array()
            .expect("statuses")
            .iter()
            .all(|status| status["actual"] == "running")
    );

    let (response, rejected) = call(
        &fixture.app,
        "POST",
        "/workloads/all/stop",
        None,
        Some(json!({})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(rejected["error"]["code"], "CSRF_REJECTED");

    let (response, stopped) = call(
        &fixture.app,
        "POST",
        "/workloads/all/stop",
        Some(&csrf),
        Some(json!({})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "{stopped}");
    assert!(
        stopped["data"]
            .as_array()
            .expect("statuses")
            .iter()
            .all(|status| status["actual"] == "stopped")
    );
    let (response, _) = call(&fixture.app, "GET", "/settings", None, None).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "control API remains available"
    );

    let (response, started) = call(
        &fixture.app,
        "POST",
        "/workloads/mcp/start",
        Some(&csrf),
        Some(json!({})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        started["data"]
            .as_array()
            .expect("statuses")
            .iter()
            .find(|status| status["component"] == "mcp")
            .is_some_and(|status| status["actual"] == "running")
    );
    let (response, restarted) = call(
        &fixture.app,
        "POST",
        "/workloads/scheduler/restart",
        Some(&csrf),
        Some(json!({})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        restarted["data"]
            .as_array()
            .expect("statuses")
            .iter()
            .all(|status| status["actual"] == "running")
    );

    let (_, audit) = call(&fixture.app, "GET", "/admin-events", None, None).await;
    let workload_events = audit["data"]
        .as_array()
        .expect("events")
        .iter()
        .filter(|event| {
            matches!(
                event["event_type"].as_str(),
                Some("workload_started" | "workload_stopped" | "workload_restarted")
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(workload_events.len(), 3);
    assert_eq!(workload_events[0]["event_type"], "workload_stopped");
    assert_eq!(workload_events[0]["details"], json!({"component":"all"}));
    assert_eq!(workload_events[1]["event_type"], "workload_started");
    assert_eq!(workload_events[1]["details"], json!({"component":"mcp"}));
    assert_eq!(workload_events[2]["event_type"], "workload_restarted");
    assert_eq!(
        workload_events[2]["details"],
        json!({"component":"scheduler"})
    );
}

#[tokio::test]
async fn overlapping_workload_requests_return_the_common_retryable_conflict_envelope() {
    let blocking = Arc::new(BlockingRuntime {
        running: AtomicBool::new(true),
        stop_entered: Notify::new(),
        release_stop: Notify::new(),
    });
    let fixture = fixture_with_runtimes(
        Arc::clone(&blocking) as Arc<dyn WorkloadRuntime>,
        Arc::new(ProbeRuntime(AtomicBool::new(true))),
    )
    .await;
    let csrf = csrf(&fixture.app).await;
    let app = fixture.app.clone();
    let first_csrf = csrf.clone();
    let first = tokio::spawn(async move {
        call(
            &app,
            "POST",
            "/workloads/mcp/stop",
            Some(&first_csrf),
            Some(json!({})),
        )
        .await
    });
    blocking.stop_entered.notified().await;

    let (response, conflict) = call(
        &fixture.app,
        "POST",
        "/workloads/scheduler/restart",
        Some(&csrf),
        Some(json!({})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(conflict["error"]["code"], "WORKLOAD_CONFLICT");
    assert_eq!(conflict["error"]["retryable"], true);
    assert_eq!(conflict["request_id"], "request-admin-test");

    blocking.release_stop.notify_one();
    let (response, _) = first.await.expect("first request");
    assert_eq!(response.status(), StatusCode::OK);

    let (_, audit) = call(&fixture.app, "GET", "/admin-events", None, None).await;
    let rejected = audit["data"]
        .as_array()
        .expect("events")
        .iter()
        .find(|event| event["outcome"] == "rejected")
        .expect("rejected event");
    assert_eq!(rejected["event_type"], "workload_restarted");
    assert_eq!(
        rejected["details"],
        json!({"component":"scheduler","failure_code":"WORKLOAD_CONFLICT"})
    );
}
