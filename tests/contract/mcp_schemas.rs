use remindi::mcp::responses::{ErrorCode, ErrorResponse, SuccessResponse, ToolError};
use remindi::mcp::schemas::{
    AddInput, CancelInput, CheckInput, CompleteInput, HistoryInput, ListInput, SnoozeInput,
    TOOL_NAMES, UpdateInput, input_schema,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

const EXPECTED_TOOLS: [&str; 8] = [
    "remindi_add",
    "remindi_check",
    "remindi_complete",
    "remindi_snooze",
    "remindi_update",
    "remindi_list",
    "remindi_cancel",
    "remindi_history",
];

#[test]
fn tool_input_inventory_is_exact_and_stable() {
    assert_eq!(TOOL_NAMES, EXPECTED_TOOLS);
}

#[test]
fn every_input_schema_is_strict_and_never_exposes_owner() {
    let schemas = [
        input_schema::<AddInput>(),
        input_schema::<CheckInput>(),
        input_schema::<CompleteInput>(),
        input_schema::<SnoozeInput>(),
        input_schema::<UpdateInput>(),
        input_schema::<ListInput>(),
        input_schema::<CancelInput>(),
        input_schema::<HistoryInput>(),
    ];

    for schema in schemas {
        assert_eq!(
            schema.get("$schema").and_then(Value::as_str),
            Some("https://json-schema.org/draft/2020-12/schema")
        );
        assert_eq!(
            schema.get("additionalProperties").and_then(Value::as_bool),
            Some(false)
        );
        assert!(!schema.to_string().contains("owner_id"));
    }
}

#[test]
fn generated_schemas_retain_source_limits_and_required_fields() {
    let add = input_schema::<AddInput>();
    assert_properties(
        &add,
        &[
            "project_id",
            "task_id",
            "message",
            "instructions",
            "priority",
            "trigger",
            "recurrence",
            "overdue_after_seconds",
            "links",
            "session_id",
            "task_lineage_id",
            "idempotency_key",
        ],
    );
    assert_eq!(at(&add, "/properties/message/minLength"), &json!(1));
    assert_eq!(at(&add, "/properties/message/maxLength"), &json!(8192));
    assert_eq!(at(&add, "/properties/links/maxItems"), &json!(100));
    assert_eq!(
        at(&add, "/properties/idempotency_key/pattern"),
        &json!("^[A-Za-z0-9._:-]+$")
    );
    assert!(required(&add).contains(&"project_id"));
    assert!(required(&add).contains(&"trigger"));
    assert!(required(&add).contains(&"idempotency_key"));

    let check = input_schema::<CheckInput>();
    assert_properties(
        &check,
        &[
            "project_id",
            "task_id",
            "session_id",
            "task_lineage_id",
            "lifecycle_event",
            "active_goal_ids",
            "include_scheduled",
            "evaluate_conditions",
            "limit",
            "cursor",
        ],
    );
    assert_eq!(at(&check, "/properties/limit/minimum"), &json!(1));
    assert_eq!(at(&check, "/properties/limit/maximum"), &json!(200));
    assert_eq!(at(&check, "/properties/limit/default"), &json!(50));
    assert_eq!(
        at(&check, "/properties/active_goal_ids/maxItems"),
        &json!(1000)
    );

    let update = input_schema::<UpdateInput>();
    assert_properties(
        &update,
        &[
            "remindi_id",
            "expected_version",
            "message",
            "instructions",
            "priority",
            "trigger",
            "recurrence",
            "overdue_after_seconds",
            "links",
            "occurrence_disposition",
            "reason",
            "idempotency_key",
        ],
    );
    assert_eq!(at(&update, "/minProperties"), &json!(5));
    assert_eq!(
        at(&update, "/properties/overdue_after_seconds/maximum"),
        &json!(31_536_000)
    );

    let history = input_schema::<HistoryInput>();
    assert_eq!(
        at(&history, "/properties/after_sequence/minimum"),
        &json!(0)
    );
    assert_eq!(at(&history, "/properties/limit/default"), &json!(100));
}

#[test]
fn every_input_type_rejects_unknown_fields() {
    assert_unknown_rejected::<AddInput>(json!({
        "project_id":"p","message":"m","trigger":{"type":"next_session"},
        "idempotency_key":"abcdefgh","owner_id":"other"
    }));
    assert_unknown_rejected::<CheckInput>(json!({
        "project_id":"p","lifecycle_event":"checkpoint","extra":true
    }));
    assert_unknown_rejected::<CompleteInput>(json!({
        "remindi_id":"01d25c98-3e53-4c80-82d7-cac04d0128c7","expected_version":1,
        "evidence":{"type":"test_result","summary":"tests pass","content_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","observed_at":"2026-07-18T00:00:00Z"},
        "idempotency_key":"abcdefgh","extra":true
    }));
    assert_unknown_rejected::<SnoozeInput>(json!({
        "remindi_id":"01d25c98-3e53-4c80-82d7-cac04d0128c7","expected_version":1,
        "snooze_until":"2026-07-19T00:00:00Z","reason":"moved","idempotency_key":"abcdefgh",
        "extra":true
    }));
    assert_unknown_rejected::<UpdateInput>(json!({
        "remindi_id":"01d25c98-3e53-4c80-82d7-cac04d0128c7","expected_version":1,
        "message":"new","reason":"changed","idempotency_key":"abcdefgh","extra":true
    }));
    assert_unknown_rejected::<ListInput>(json!({"extra":true}));
    assert_unknown_rejected::<CancelInput>(json!({
        "remindi_id":"01d25c98-3e53-4c80-82d7-cac04d0128c7","expected_version":1,
        "reason":"obsolete","idempotency_key":"abcdefgh","extra":true
    }));
    assert_unknown_rejected::<HistoryInput>(json!({
        "remindi_id":"01d25c98-3e53-4c80-82d7-cac04d0128c7","extra":true
    }));
}

#[test]
fn semantic_hooks_reject_cross_field_contract_violations() {
    let add: AddInput = serde_json::from_value(json!({
        "project_id":"p","message":"m","trigger":{"type":"at_time","at":"2026-07-19T00:00:00Z"},
        "recurrence":{"every_seconds":60},"idempotency_key":"abcdefgh"
    }))
    .expect("structural add input");
    assert!(add.validate_semantics().is_err());

    let update: UpdateInput = serde_json::from_value(json!({
        "remindi_id":"01d25c98-3e53-4c80-82d7-cac04d0128c7",
        "expected_version":1,"reason":"no change","idempotency_key":"abcdefgh"
    }))
    .expect("structural update input");
    assert!(update.validate_semantics().is_err());
}

#[test]
fn response_envelopes_have_exact_success_and_error_fields() {
    let success = serde_json::to_value(SuccessResponse::new(
        "req_01",
        json!({"remindi":{"id":"01d25c98-3e53-4c80-82d7-cac04d0128c7","state":"due","version":2}}),
    ))
    .expect("serialize success");
    assert_eq!(
        object_keys(&success),
        ["data", "ok", "request_id"].map(str::to_owned)
    );
    assert_eq!(success["ok"], json!(true));

    let failure = serde_json::to_value(ErrorResponse::new(
        "req_02",
        ToolError::new(ErrorCode::VersionConflict, "changed")
            .with_details(json!({"current_version":7})),
    ))
    .expect("serialize error");
    assert_eq!(
        object_keys(&failure),
        ["error", "ok", "request_id"].map(str::to_owned)
    );
    assert_eq!(
        object_keys(&failure["error"]),
        ["code", "details", "message", "retryable"].map(str::to_owned)
    );
    assert_eq!(failure["error"]["retryable"], json!(true));
}

#[test]
fn error_code_retryability_matches_the_specification() {
    use ErrorCode::{
        AdapterDisabled, AdapterError, AdapterNotFound, AdapterTimeout, BackupInvalid,
        CsrfRejected, DatabaseBusy, Forbidden, IdempotencyKeyReused, InternalError, InvalidState,
        LimitExceeded, MaintenanceActive, NotFound, ReauthenticationRequired, RestoreFailed,
        Unauthenticated, ValidationError, VersionConflict, WorkloadConflict,
    };

    for code in [
        ValidationError,
        Unauthenticated,
        Forbidden,
        NotFound,
        InvalidState,
        IdempotencyKeyReused,
        AdapterNotFound,
        AdapterDisabled,
        ReauthenticationRequired,
        CsrfRejected,
        BackupInvalid,
        LimitExceeded,
    ] {
        assert_eq!(code.retryable(), false.into());
    }
    for code in [
        VersionConflict,
        DatabaseBusy,
        AdapterTimeout,
        WorkloadConflict,
        MaintenanceActive,
    ] {
        assert_eq!(code.retryable(), true.into());
    }
    for code in [AdapterError, RestoreFailed, InternalError] {
        assert!(code.retryable().is_conditional());
    }
}

fn assert_unknown_rejected<T: DeserializeOwned>(value: Value) {
    assert!(serde_json::from_value::<T>(value).is_err());
}

fn at<'a>(schema: &'a Value, pointer: &str) -> &'a Value {
    schema
        .pointer(pointer)
        .unwrap_or_else(|| panic!("missing {pointer}"))
}

fn required(schema: &Value) -> Vec<&str> {
    schema["required"]
        .as_array()
        .expect("required array")
        .iter()
        .map(|value| value.as_str().expect("required string"))
        .collect()
}

fn object_keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<_> = value.as_object().expect("object").keys().cloned().collect();
    keys.sort();
    keys
}

fn assert_properties(schema: &Value, expected: &[&str]) {
    let properties = schema["properties"].as_object().expect("properties object");
    let mut actual: Vec<_> = properties.keys().map(String::as_str).collect();
    actual.sort_unstable();
    let mut expected = expected.to_vec();
    expected.sort_unstable();
    assert_eq!(actual, expected);
}
