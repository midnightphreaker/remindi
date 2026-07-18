use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use axum::{
    body::Body,
    http::{Request, Response, StatusCode, header},
};
use http_body_util::BodyExt;
use remindi::{
    auth::web_session::WebSessionManager,
    clock::{FixedClock, IdGenerator},
    config::BootstrapConfig,
    db::DatabaseManager,
    http::api::{WebApiState, router},
    remindi::RemindiService,
};
use serde_json::{Value, json};
use time::macros::datetime;
use tower::ServiceExt;
use uuid::Uuid;

#[derive(Default)]
struct SequenceIds(AtomicU64);

impl IdGenerator for SequenceIds {
    fn next_id(&self) -> Uuid {
        Uuid::from_u128(u128::from(self.0.fetch_add(1, Ordering::Relaxed) + 1))
    }
}

async fn app() -> axum::Router {
    let config = Arc::new(
        BootstrapConfig::from_pairs([
            ("REMINDI_OWNER_ID", "owner-a"),
            ("REMINDI_MCP_TOKEN", "mcp-token"),
            ("REMINDI_WEBUI_AUTH", "false"),
        ])
        .expect("config"),
    );
    let directory = std::env::temp_dir().join(format!("remindi-web-api-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&directory).expect("temp directory");
    let database = Arc::new(
        DatabaseManager::open(directory.join("remindi.db"))
            .await
            .expect("database"),
    );
    let service = Arc::new(RemindiService::new(
        database,
        config.owner_id(),
        b"cursor-key",
        Arc::new(FixedClock::new(datetime!(2026-07-19 06:00 UTC))),
        Arc::new(SequenceIds::default()),
    ));
    router(WebApiState::new(
        WebSessionManager::from_config(&config).expect("sessions"),
        service,
    ))
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    csrf: Option<&str>,
    body: Option<Value>,
) -> (Response<Body>, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "remindi.local")
        .header(header::ORIGIN, "http://remindi.local");
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
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let (parts, body) = response.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (Response::from_parts(parts, Body::empty()), value)
}

async fn csrf(app: &axum::Router) -> String {
    let (_, body) = call(app, "GET", "/session", None, None).await;
    assert_eq!(body["data"]["actor_id"], "webui:unauthenticated");
    assert_eq!(body["data"]["authentication_required"], false);
    body["data"]["csrf_token"].as_str().unwrap().to_owned()
}

fn add_body(key: &str, message: &str) -> Value {
    json!({
        "project_id": "project-a",
        "task_id": "task-a",
        "message": message,
        "instructions": null,
        "priority": "high",
        "trigger": {"type": "at_time", "at": "2026-07-19T05:00:00Z"},
        "overdue_after_seconds": 60,
        "links": [],
        "idempotency_key": key
    })
}

#[tokio::test]
async fn all_eight_operations_share_service_semantics() {
    let app = app().await;
    let csrf = csrf(&app).await;

    let (response, add) = call(
        &app,
        "POST",
        "/remindi",
        Some(&csrf),
        Some(add_body("create-0001", "First reminder")),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let first = add["data"]["remindi"]["id"].as_str().unwrap();

    let (_, check) = call(
        &app,
        "POST",
        "/remindi/check",
        Some(&csrf),
        Some(json!({
            "project_id": "project-a",
            "task_id": "task-a",
            "lifecycle_event": "checkpoint"
        })),
    )
    .await;
    assert_eq!(check["data"]["items"][0]["remindi_id"], first);

    let (_, snooze) = call(
        &app,
        "POST",
        &format!("/remindi/{first}/snooze"),
        Some(&csrf),
        Some(json!({
            "expected_version": check["data"]["items"][0]["version"],
            "snooze_until": "2026-07-19T07:00:00Z",
            "reason": "Wait for the next window",
            "idempotency_key": "snooze-0001"
        })),
    )
    .await;
    assert_eq!(snooze["ok"], true);

    let (_, updated) = call(
        &app,
        "PATCH",
        &format!("/remindi/{first}"),
        Some(&csrf),
        Some(json!({
            "expected_version": snooze["data"]["remindi"]["version"],
            "message": "Updated reminder",
            "instructions": "Use the safe sequence",
            "priority": "critical",
            "reason": "Clarify the action",
            "idempotency_key": "update-0001"
        })),
    )
    .await;
    assert_eq!(updated["data"]["remindi"]["message"], "Updated reminder");

    let (_, listed) = call(
        &app,
        "GET",
        "/remindi?project_id=project-a&states=snoozed",
        None,
        None,
    )
    .await;
    assert_eq!(listed["data"]["items"].as_array().unwrap().len(), 1);

    let (_, second) = call(
        &app,
        "POST",
        "/remindi",
        Some(&csrf),
        Some(add_body("create-0002", "Complete me")),
    )
    .await;
    let second_id = second["data"]["remindi"]["id"].as_str().unwrap();
    let (_, checked) = call(
        &app,
        "POST",
        "/remindi/check",
        Some(&csrf),
        Some(json!({"project_id":"project-a","task_id":"task-a","lifecycle_event":"checkpoint"})),
    )
    .await;
    let version = checked["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["remindi_id"] == second_id)
        .unwrap()["version"]
        .clone();
    let (_, completed) = call(
        &app,
        "POST",
        &format!("/remindi/{second_id}/complete"),
        Some(&csrf),
        Some(json!({
            "expected_version": version,
            "evidence": {
                "type": "observation",
                "summary": "Web API acceptance passed",
                "reference_uri": "https://example.invalid/run/1",
                "observed_at": "2026-07-19T06:00:00Z"
            },
            "idempotency_key": "complete-0001"
        })),
    )
    .await;
    assert_eq!(completed["data"]["remindi"]["state"], "completed");

    let (_, third) = call(
        &app,
        "POST",
        "/remindi",
        Some(&csrf),
        Some(add_body("create-0003", "Cancel me")),
    )
    .await;
    let third_id = third["data"]["remindi"]["id"].as_str().unwrap();
    let (_, cancelled) = call(
        &app,
        "POST",
        &format!("/remindi/{third_id}/cancel"),
        Some(&csrf),
        Some(json!({
            "expected_version": 1,
            "reason": "No longer required",
            "idempotency_key": "cancel-0001"
        })),
    )
    .await;
    assert_eq!(cancelled["data"]["remindi"]["state"], "cancelled");

    let (_, history) = call(
        &app,
        "GET",
        &format!("/remindi/{second_id}/history"),
        None,
        None,
    )
    .await;
    assert_eq!(
        history["data"]["completion_evidence"]["summary"],
        "Web API acceptance passed"
    );
}

#[tokio::test]
async fn every_mutation_rejects_missing_origin_or_csrf_and_body_limit_is_bounded() {
    let app = app().await;
    let id = Uuid::now_v7();
    let cases = vec![
        (
            "POST",
            "/remindi".to_owned(),
            Some(add_body("create-0001", "Rejected")),
        ),
        (
            "POST",
            "/remindi/check".to_owned(),
            Some(json!({"project_id":"project-a","lifecycle_event":"checkpoint"})),
        ),
        (
            "POST",
            format!("/remindi/{id}/complete"),
            Some(json!({
                "expected_version":1,
                "evidence":{"type":"test_result","summary":"Evidence exists","reference_uri":"https://example.invalid/evidence","observed_at":"2026-07-19T06:00:00Z"},
                "idempotency_key":"complete-0001"
            })),
        ),
        (
            "POST",
            format!("/remindi/{id}/snooze"),
            Some(
                json!({"expected_version":1,"snooze_until":"2026-07-19T07:00:00Z","reason":"later","idempotency_key":"snooze-0001"}),
            ),
        ),
        (
            "PATCH",
            format!("/remindi/{id}"),
            Some(
                json!({"expected_version":1,"message":"updated","reason":"because","idempotency_key":"update-0001"}),
            ),
        ),
        (
            "POST",
            format!("/remindi/{id}/cancel"),
            Some(json!({"expected_version":1,"reason":"cancel","idempotency_key":"cancel-0001"})),
        ),
        ("POST", "/auth/logout".to_owned(), None),
    ];
    for (method, uri, payload) in cases {
        let (response, body) = call(&app, method, &uri, None, payload).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{method} {uri}");
        assert_eq!(body["error"]["code"], "CSRF_REJECTED", "{method} {uri}");
    }

    let oversized = "x".repeat(1024 * 1024 + 1);
    let csrf = csrf(&app).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/remindi")
                .header(header::HOST, "remindi.local")
                .header(header::ORIGIN, "http://remindi.local")
                .header("x-csrf-token", csrf)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(oversized))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn nested_payloads_and_queries_reject_unknown_fields() {
    let app = app().await;
    let csrf = csrf(&app).await;

    let mut trigger = add_body("unknown-0001", "Unknown trigger field");
    trigger["trigger"]["unexpected"] = json!(true);
    let mut recurrence = add_body("unknown-0002", "Unknown recurrence field");
    recurrence["trigger"] =
        json!({"type":"interval","first_at":"2026-07-19T07:00:00Z","every_seconds":60});
    recurrence["recurrence"] =
        json!({"every_seconds":60,"missed_policy":"coalesce","unexpected":true});
    let mut link = add_body("unknown-0003", "Unknown link field");
    link["links"] = json!([{"type":"issue","value":"issue-1","unexpected":true}]);

    for payload in [trigger, recurrence, link] {
        let (response, body) = call(&app, "POST", "/remindi", Some(&csrf), Some(payload)).await;
        assert!(response.status().is_client_error());
        assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
    }

    let id = Uuid::now_v7();
    let (response, body) = call(
        &app,
        "POST",
        &format!("/remindi/{id}/complete"),
        Some(&csrf),
        Some(json!({
            "expected_version":1,
            "evidence":{
                "type":"test_result",
                "summary":"Evidence exists",
                "reference_uri":"https://example.invalid/evidence",
                "observed_at":"2026-07-19T06:00:00Z",
                "unexpected":true
            },
            "idempotency_key":"complete-unknown"
        })),
    )
    .await;
    assert!(response.status().is_client_error());
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");

    let (response, body) = call(&app, "GET", "/remindi?unexpected=true", None, None).await;
    assert!(response.status().is_client_error());
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
    let (response, body) = call(
        &app,
        "GET",
        &format!("/remindi/{id}/history?unexpected=true"),
        None,
        None,
    )
    .await;
    assert!(response.status().is_client_error());
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
}
