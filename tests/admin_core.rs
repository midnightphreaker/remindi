use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use remindi::{
    admin::{
        AdminActor, AdminError, AdminService,
        adapters::{AdapterConfiguration, HttpAliasConfiguration, TcpAliasConfiguration},
    },
    clock::{FixedClock, UuidV7Generator},
    config::BootstrapConfig,
    db::DatabaseManager,
    scheduler::AdapterProvider,
};
use serde_json::{Value, json};
use time::macros::datetime;
use uuid::Uuid;

fn temporary_directory(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("remindi-admin-{label}-{}", Uuid::now_v7()));
    fs::create_dir_all(&path).expect("temporary directory is created");
    path
}

fn bootstrap(root: &Path) -> BootstrapConfig {
    BootstrapConfig::from_pairs([
        (
            "REMINDI_DB_PATH",
            root.join("remindi.db").to_str().expect("UTF-8 path"),
        ),
        ("REMINDI_OWNER_ID", "owner-private"),
        ("REMINDI_MCP_TOKEN", "mcp-super-secret"),
        (
            "REMINDI_BACKUP_DIR",
            root.join("backups").to_str().expect("UTF-8 path"),
        ),
        ("REMINDI_WEBUI_AUTH", "false"),
        ("REMINDI_WEBUI_USERNAME", "admin-private"),
        ("REMINDI_WEBUI_PASSWORD", "webui-super-secret"),
        ("REMINDI_HTTP_ALLOWED_HOSTS", "private.example"),
        ("REMINDI_WEBUI_TITLE", "Private Remindi"),
    ])
    .expect("bootstrap configuration")
}

async fn service(label: &str) -> (AdminService, Arc<DatabaseManager>) {
    let root = temporary_directory(label);
    let bootstrap = Arc::new(bootstrap(&root));
    let database = Arc::new(
        DatabaseManager::open(bootstrap.database_path())
            .await
            .expect("database opens"),
    );
    let service = AdminService::load(
        Arc::clone(&database),
        bootstrap,
        Arc::new(FixedClock::new(datetime!(2026-07-18 13:00 UTC))),
        Arc::new(UuidV7Generator),
    )
    .await
    .expect("admin service loads");
    (service, database)
}

fn actor() -> AdminActor {
    AdminActor::new("web:owner", Some("req-admin-1".to_owned()))
        .expect("bounded authenticated actor")
}

#[tokio::test]
async fn bootstrap_view_redacts_credentials_identity_paths_and_allowlists() {
    let (service, _) = service("bootstrap-redaction").await;

    let encoded = serde_json::to_string(&service.bootstrap_view()).expect("view serializes");

    for private in [
        "mcp-super-secret",
        "webui-super-secret",
        "admin-private",
        "owner-private",
        "private.example",
        "/remindi.db",
        "/backups",
    ] {
        assert!(
            !encoded.contains(private),
            "bootstrap response exposed private value matching {private}"
        );
    }
}

#[tokio::test]
async fn runtime_setting_inventory_is_exact_versioned_and_immediate() {
    let (service, _) = service("runtime-inventory").await;

    let settings = service.runtime_settings().await.expect("settings load");
    let summary = settings
        .iter()
        .map(|setting| {
            (
                setting.key.as_str(),
                setting.value,
                setting.minimum,
                setting.maximum,
                setting.version,
                setting.restart_required,
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        summary,
        vec![
            ("adapters.max_concurrency", 8, 1, None, 1, false),
            ("adapters.timeout_seconds", 5, 1, None, 1, false),
            ("backups.interval_seconds", 86_400, 1, None, 1, false),
            ("backups.retention_count", 14, 1, None, 1, false),
            ("backups.upload_max_bytes", 1_073_741_824, 1, None, 1, false),
            ("idempotency.retention_days", 30, 1, None, 1, false),
            ("recurrence.max_catch_up_occurrences", 10, 1, None, 1, false),
            ("remindi.default_overdue_seconds", 0, 0, None, 1, false),
            ("remindi.max_snooze_seconds", 31_536_000, 1, None, 1, false),
            ("scheduler.lease_seconds", 90, 1, None, 1, false),
            ("scheduler.poll_interval_seconds", 30, 1, None, 1, false),
        ]
    );
}

#[tokio::test]
async fn runtime_setting_update_enforces_version_and_scheduler_relation() {
    let (service, database) = service("runtime-version").await;

    let updated = service
        .update_runtime_setting("scheduler.poll_interval_seconds", 40, 1, &actor())
        .await
        .expect("valid setting update");
    let conflict = service
        .update_runtime_setting("scheduler.poll_interval_seconds", 41, 1, &actor())
        .await
        .expect_err("stale version is rejected");
    let invalid = service
        .update_runtime_setting("scheduler.lease_seconds", 80, 1, &actor())
        .await
        .expect_err("lease must exceed twice the poll interval");

    assert_eq!(updated.version, 2);
    assert_eq!(conflict, AdminError::VersionConflict);
    assert_eq!(invalid, AdminError::Validation);

    let mut connection = database.connection().await.expect("database connection");
    let outcomes: Vec<String> = sqlx::query_scalar(
        "SELECT outcome FROM admin_events \
         WHERE event_type = 'runtime_setting_updated' ORDER BY sequence",
    )
    .fetch_all(connection.as_mut())
    .await
    .expect("audit outcomes");
    assert_eq!(outcomes, vec!["succeeded", "rejected", "rejected"]);
}

#[tokio::test]
async fn unknown_setting_is_rejected_without_recording_arbitrary_input() {
    let (service, database) = service("runtime-unknown").await;
    let secret_like_key = "unknown.secret.mcp-super-secret";

    let result = service
        .update_runtime_setting(secret_like_key, 1, 1, &actor())
        .await;

    assert_eq!(
        result.expect_err("unknown key is rejected"),
        AdminError::Validation
    );
    let mut connection = database.connection().await.expect("database connection");
    let details: String = sqlx::query_scalar(
        "SELECT details_json FROM admin_events \
         WHERE event_type = 'runtime_setting_updated' ORDER BY sequence DESC LIMIT 1",
    )
    .fetch_one(connection.as_mut())
    .await
    .expect("audit details");
    assert_eq!(
        serde_json::from_str::<Value>(&details).expect("valid details"),
        json!({
            "failure_code": "VALIDATION_ERROR",
            "setting_name": "[unknown]"
        })
    );
    assert!(!details.contains("mcp-super-secret"));
}

#[tokio::test]
async fn adapter_update_validates_full_candidate_before_atomic_publication() {
    let (service, database) = service("adapter-atomic").await;
    let adapters = service.adapters();
    let initial = adapters
        .get("http_health")
        .expect("registered HTTP adapter");

    let invalid = AdapterConfiguration::HttpHealth {
        aliases: BTreeMap::from([(
            "bad alias".to_owned(),
            HttpAliasConfiguration {
                url: "https://secret.example/health".to_owned(),
                expected_statuses: vec![200],
                max_response_bytes: 1024,
                expected_content_type: None,
                allow_redirects: false,
                allow_private: false,
            },
        )]),
    };
    let result = service
        .update_adapter("http_health", true, invalid, 1, &actor())
        .await;

    assert_eq!(
        result.expect_err("invalid alias is rejected"),
        AdminError::Validation
    );
    assert!(
        Arc::ptr_eq(
            &initial,
            &adapters
                .get("http_health")
                .expect("HTTP adapter remains published")
        ),
        "invalid candidate must not replace the active snapshot"
    );
    let mut connection = database.connection().await.expect("database connection");
    let row: (i64, i64, String) = sqlx::query_as(
        "SELECT enabled, version, config_json FROM adapter_configs \
         WHERE adapter_name = 'http_health'",
    )
    .fetch_one(connection.as_mut())
    .await
    .expect("persisted adapter row");
    assert_eq!(row, (0, 1, "{}".to_owned()));

    let details: String = sqlx::query_scalar(
        "SELECT details_json FROM admin_events \
         WHERE event_type = 'adapter_config_updated' ORDER BY sequence DESC LIMIT 1",
    )
    .fetch_one(connection.as_mut())
    .await
    .expect("audit details");
    assert!(!details.contains("secret.example"));
    assert!(!details.contains("bad alias"));
}

#[tokio::test]
async fn all_four_typed_adapter_configs_persist_and_publish_with_one_version_step() {
    let (service, _) = service("adapter-four").await;
    let root = temporary_directory("file-root");
    let file = root.join("ready.flag");
    fs::write(&file, b"ready").expect("fixture file");

    let updates = [
        (
            "observation_window_ended",
            AdapterConfiguration::ObservationWindowEnded,
        ),
        (
            "http_health",
            AdapterConfiguration::HttpHealth {
                aliases: BTreeMap::from([(
                    "home".to_owned(),
                    HttpAliasConfiguration {
                        url: "https://example.com/health".to_owned(),
                        expected_statuses: vec![200, 204],
                        max_response_bytes: 4096,
                        expected_content_type: Some("application/json".to_owned()),
                        allow_redirects: false,
                        allow_private: false,
                    },
                )]),
            },
        ),
        (
            "tcp_reachable",
            AdapterConfiguration::TcpReachable {
                aliases: BTreeMap::from([(
                    "dns".to_owned(),
                    TcpAliasConfiguration {
                        host: "example.com".to_owned(),
                        port: 443,
                        allow_private: false,
                    },
                )]),
            },
        ),
        (
            "file_exists",
            AdapterConfiguration::FileExists {
                roots: vec![root],
                aliases: BTreeMap::from([("ready".to_owned(), file)]),
            },
        ),
    ];

    for (name, config) in updates {
        let updated = service
            .update_adapter(name, true, config, 1, &actor())
            .await
            .expect("valid adapter update");
        assert_eq!(updated.version, 2, "{name} version");
        assert!(service.adapters().get(name).is_some(), "{name} published");
    }

    let configs = service.adapter_configs().await.expect("adapter configs");
    assert_eq!(
        configs
            .iter()
            .map(|config| config.adapter_name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "file_exists",
            "http_health",
            "observation_window_ended",
            "tcp_reachable"
        ]
    );
}

#[tokio::test]
async fn successful_admin_audit_contains_only_source_defined_redacted_fields() {
    let (service, database) = service("audit-redaction").await;
    service
        .update_runtime_setting("adapters.timeout_seconds", 6, 1, &actor())
        .await
        .expect("setting update");

    let mut connection = database.connection().await.expect("database connection");
    let row: (String, String, Option<String>, String, String) = sqlx::query_as(
        "SELECT actor_id, event_type, request_id, outcome, details_json \
         FROM admin_events ORDER BY sequence DESC LIMIT 1",
    )
    .fetch_one(connection.as_mut())
    .await
    .expect("admin event");
    assert_eq!(
        (
            row.0.as_str(),
            row.1.as_str(),
            row.2.as_deref(),
            row.3.as_str(),
            serde_json::from_str::<Value>(&row.4).expect("valid details"),
        ),
        (
            "web:owner",
            "runtime_setting_updated",
            Some("req-admin-1"),
            "succeeded",
            json!({"setting_name": "adapters.timeout_seconds"}),
        )
    );
    assert!(
        !row.4.contains('6'),
        "audit must not contain setting values"
    );
}
