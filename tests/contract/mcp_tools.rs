use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use remindi::{
    clock::{FixedClock, IdGenerator},
    db::DatabaseManager,
    mcp::{McpServer, schemas::TOOL_NAMES},
    remindi::{Actor, RemindiService},
};
use serde_json::{Value, json};
use time::macros::datetime;
use uuid::Uuid;

#[derive(Default)]
struct SequenceIds(AtomicU64);

impl IdGenerator for SequenceIds {
    fn next_id(&self) -> Uuid {
        Uuid::from_u128(self.0.fetch_add(1, Ordering::Relaxed).into())
    }
}

#[test]
fn discovery_exposes_exactly_eight_stable_tools_with_complete_contracts() {
    let tools = McpServer::tool_definitions();
    let names: Vec<_> = tools.iter().map(|tool| tool.name.as_ref()).collect();

    assert_eq!(names, TOOL_NAMES);
    for tool in &tools {
        assert!(tool.title.as_ref().is_some_and(|title| !title.is_empty()));
        assert!(
            tool.description
                .as_ref()
                .is_some_and(|description| !description.is_empty())
        );
        assert_eq!(
            tool.input_schema.get("$schema").and_then(Value::as_str),
            Some("https://json-schema.org/draft/2020-12/schema")
        );
        assert!(tool.output_schema.is_some());
        assert!(tool.annotations.is_some());
        assert!(
            !serde_json::to_string(&tool.input_schema)
                .expect("schema serializes")
                .contains("owner_id")
        );
    }
}

#[test]
fn annotations_match_the_eight_tools_semantics() {
    let tools = McpServer::tool_definitions();
    let hints: Vec<_> = tools
        .iter()
        .map(|tool| {
            let annotations = tool.annotations.as_ref().expect("annotations");
            (
                tool.name.as_ref(),
                annotations.read_only_hint,
                annotations.destructive_hint,
                annotations.idempotent_hint,
                annotations.open_world_hint,
            )
        })
        .collect();

    assert_eq!(
        hints,
        vec![
            (
                "remindi_add",
                Some(false),
                Some(false),
                Some(true),
                Some(false)
            ),
            (
                "remindi_check",
                Some(false),
                Some(false),
                Some(true),
                Some(true)
            ),
            (
                "remindi_complete",
                Some(false),
                Some(true),
                Some(true),
                Some(false)
            ),
            (
                "remindi_snooze",
                Some(false),
                Some(true),
                Some(true),
                Some(false)
            ),
            (
                "remindi_update",
                Some(false),
                Some(true),
                Some(true),
                Some(false)
            ),
            (
                "remindi_list",
                Some(true),
                Some(false),
                Some(true),
                Some(false)
            ),
            (
                "remindi_cancel",
                Some(false),
                Some(true),
                Some(true),
                Some(false)
            ),
            (
                "remindi_history",
                Some(true),
                Some(false),
                Some(true),
                Some(false)
            ),
        ]
    );
}

async fn server() -> (McpServer, Arc<DatabaseManager>) {
    let directory = std::env::temp_dir().join(format!("remindi-mcp-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&directory).expect("temporary directory");
    let database = Arc::new(
        DatabaseManager::open(&directory.join("remindi.db"))
            .await
            .expect("database opens"),
    );
    let service = Arc::new(RemindiService::new(
        Arc::clone(&database),
        "owner-a",
        b"mcp-contract-secret",
        Arc::new(FixedClock::new(datetime!(2026-07-19 06:00 UTC))),
        Arc::new(SequenceIds::default()),
    ));
    (
        McpServer::new(service, || {
            Actor::agent("authenticated-agent", Some("request-a".into()))
        }),
        database,
    )
}

async fn call_ok(server: &McpServer, name: &str, arguments: Value) -> Value {
    let result = server.execute(name, arguments).await;
    assert_eq!(result.is_error, Some(false), "{name} failed: {result:?}");
    result.structured_content.expect("structured result")
}

#[tokio::test]
async fn lifecycle_tools_share_service_state_and_read_tools_do_not_mutate_it() {
    let (server, database) = server().await;
    let add = call_ok(
        &server,
        "remindi_add",
        json!({
            "project_id": "project-a",
            "message": "Run the due lifecycle",
            "trigger": {"type": "at_time", "at": "2026-07-19T05:00:00Z"},
            "idempotency_key": "lifecycle-add"
        }),
    )
    .await;
    let id = add["data"]["remindi"]["id"].as_str().expect("item id");

    let checked = call_ok(
        &server,
        "remindi_check",
        json!({"project_id": "project-a", "lifecycle_event": "checkpoint"}),
    )
    .await;
    assert_eq!(checked["data"]["items"][0]["readiness"], "due");
    let due_version = checked["data"]["items"][0]["version"]
        .as_u64()
        .expect("due version");

    let updated = call_ok(
        &server,
        "remindi_update",
        json!({
            "remindi_id": id,
            "expected_version": due_version,
            "priority": "high",
            "reason": "Raise priority",
            "idempotency_key": "lifecycle-update"
        }),
    )
    .await;
    let updated_version = updated["data"]["remindi"]["version"]
        .as_u64()
        .expect("updated version");

    let snoozed = call_ok(
        &server,
        "remindi_snooze",
        json!({
            "remindi_id": id,
            "expected_version": updated_version,
            "snooze_until": "2026-07-19T07:00:00Z",
            "reason": "Maintenance moved",
            "idempotency_key": "lifecycle-snooze"
        }),
    )
    .await;
    assert_eq!(snoozed["data"]["remindi"]["state"], "snoozed");
    let snoozed_version = snoozed["data"]["remindi"]["version"]
        .as_u64()
        .expect("snoozed version");

    let listed = call_ok(&server, "remindi_list", json!({"project_id": "project-a"})).await;
    assert_eq!(listed["data"]["items"][0]["id"], id);
    assert!(listed["data"]["items"][0].get("owner_id").is_none());
    assert_eq!(
        listed["data"]["items"][0]["created_at"],
        "2026-07-19T06:00:00.000Z"
    );
    assert_eq!(
        listed["data"]["items"][0]["snooze_until"],
        "2026-07-19T07:00:00.000Z"
    );
    assert_eq!(
        listed["data"]["items"][0]["trigger"]["at"],
        "2026-07-19T05:00:00.000Z"
    );
    let history = call_ok(&server, "remindi_history", json!({"remindi_id": id})).await;
    assert!(
        history["data"]["events"]
            .as_array()
            .is_some_and(|events| events.len() >= 4)
    );
    assert!(
        history["data"]["events"]
            .as_array()
            .expect("events")
            .iter()
            .all(|event| event["occurred_at"] == "2026-07-19T06:00:00.000Z")
    );
    let snooze_event = history["data"]["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|event| event["event_type"] == "snoozed")
        .expect("snooze event");
    assert_eq!(
        snooze_event["details"]["snooze_until"],
        "2026-07-19T07:00:00.000Z"
    );

    let cancelled = call_ok(
        &server,
        "remindi_cancel",
        json!({
            "remindi_id": id,
            "expected_version": snoozed_version,
            "reason": "No longer required",
            "idempotency_key": "lifecycle-cancel"
        }),
    )
    .await;
    assert_eq!(cancelled["data"]["remindi"]["state"], "cancelled");

    drop(server);
    Arc::try_unwrap(database)
        .expect("sole database owner")
        .close()
        .await
        .expect("database closes");
}

#[tokio::test]
async fn complete_accepts_authenticated_structured_evidence() {
    let (server, database) = server().await;
    let add = call_ok(
        &server,
        "remindi_add",
        json!({
            "project_id": "project-a",
            "message": "Complete with evidence",
            "trigger": {"type": "next_session"},
            "idempotency_key": "complete-add"
        }),
    )
    .await;
    let id = add["data"]["remindi"]["id"].as_str().expect("item id");
    let completed = call_ok(
        &server,
        "remindi_complete",
        json!({
            "remindi_id": id,
            "expected_version": 1,
            "evidence": {
                "type": "test_result",
                "summary": "Targeted test passed",
                "content_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "observed_at": "2026-07-19T06:00:00Z",
                "metadata": {"next_fire_at": [2026, 200, 6, 0, 0, 0, 0, 0, 0]}
            },
            "idempotency_key": "complete-item"
        }),
    )
    .await;
    assert_eq!(completed["data"]["remindi"]["state"], "completed");
    let history = call_ok(&server, "remindi_history", json!({"remindi_id": id})).await;
    assert_eq!(
        history["data"]["completion_evidence"][0]["observed_at"],
        "2026-07-19T06:00:00.000Z"
    );
    assert_eq!(
        history["data"]["completion_evidence"][0]["recorded_at"],
        "2026-07-19T06:00:00.000Z"
    );
    assert_eq!(
        history["data"]["completion_evidence"][0]["metadata"],
        json!({"next_fire_at": [2026, 200, 6, 0, 0, 0, 0, 0, 0]})
    );

    drop(server);
    Arc::try_unwrap(database)
        .expect("sole database owner")
        .close()
        .await
        .expect("database closes");
}

#[tokio::test]
async fn add_calls_the_shared_service_and_returns_matching_structured_and_text_json() {
    let (server, database) = server().await;
    let result = server
        .execute(
            "remindi_add",
            json!({
                "project_id": "project-a",
                "message": "Collect acceptance evidence",
                "trigger": {"type": "at_time", "at": "2026-07-19T07:00:00Z"},
                "idempotency_key": "request-0001"
            }),
        )
        .await;

    assert_eq!(result.is_error, Some(false));
    let structured = result.structured_content.expect("structured content");
    assert_eq!(structured["ok"], true);
    assert_eq!(structured["request_id"], "request-a");
    assert_eq!(structured["data"]["remindi"]["state"], "scheduled");
    let text: Value =
        serde_json::from_str(&result.content[0].as_text().expect("text fallback").text)
            .expect("fallback JSON");
    assert_eq!(text, structured);

    drop(server);
    Arc::try_unwrap(database)
        .expect("sole database owner")
        .close()
        .await
        .expect("database closes");
}

#[tokio::test]
async fn invalid_payload_returns_the_safe_structured_error_envelope() {
    let (server, database) = server().await;
    let result = server
        .execute(
            "remindi_add",
            json!({
                "project_id": "project-a",
                "message": "Collect acceptance evidence",
                "trigger": {"type": "next_session"},
                "idempotency_key": "request-0002",
                "owner_id": "caller-must-not-select-owner"
            }),
        )
        .await;

    assert_eq!(result.is_error, Some(true));
    let structured = result.structured_content.expect("structured error");
    assert_eq!(structured["ok"], false);
    assert_eq!(structured["request_id"], "request-a");
    assert_eq!(structured["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(structured["error"]["retryable"], false);
    let text: Value =
        serde_json::from_str(&result.content[0].as_text().expect("text fallback").text)
            .expect("fallback JSON");
    assert_eq!(text, structured);

    drop(server);
    Arc::try_unwrap(database)
        .expect("sole database owner")
        .close()
        .await
        .expect("database closes");
}
