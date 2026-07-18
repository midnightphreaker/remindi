use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use http::{HeaderName, HeaderValue};
use remindi::{
    app::{AppState, run},
    clock::{FixedClock, IdGenerator},
    config::BootstrapConfig,
    db::DatabaseManager,
    http::router::build_router,
    mcp::{schemas::TOOL_NAMES, server::McpWorkload},
};
use rmcp::{
    ServiceExt,
    model::CallToolRequestParams,
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::json;
use time::macros::datetime;
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const TOKEN: &str = "task-six-mcp-token";
const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"remindi-contract","version":"1.0"}}}"#;

#[derive(Default)]
struct SequenceIds(AtomicU64);

impl IdGenerator for SequenceIds {
    fn next_id(&self) -> Uuid {
        Uuid::from_u128(self.0.fetch_add(1, Ordering::Relaxed).into())
    }
}

struct Fixture {
    base_url: String,
    workload: Arc<McpWorkload>,
    shutdown: CancellationToken,
    server: JoinHandle<std::io::Result<()>>,
}

impl Fixture {
    async fn stop(self) {
        self.shutdown.cancel();
        self.server
            .await
            .expect("server task joins")
            .expect("server drains");
    }
}

async fn fixture() -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("local address");
    let allowed_host = address.to_string();
    let directory = std::env::temp_dir().join(format!("remindi-transport-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&directory).expect("temporary directory");
    let config = Arc::new(
        BootstrapConfig::from_pairs([
            ("REMINDI_OWNER_ID", "owner-a"),
            ("REMINDI_MCP_TOKEN", TOKEN),
            (
                "REMINDI_DB_PATH",
                directory.join("remindi.db").to_str().expect("UTF-8 path"),
            ),
            ("REMINDI_HTTP_ALLOWED_HOSTS", allowed_host.as_str()),
            ("REMINDI_WEBUI_ENABLE", "false"),
        ])
        .expect("test configuration"),
    );
    let database = Arc::new(
        DatabaseManager::open(config.database_path())
            .await
            .expect("database opens"),
    );
    let state = AppState::new(
        config,
        Arc::new(FixedClock::new(datetime!(2026-07-19 06:00 UTC))),
        Arc::new(SequenceIds::default()),
    )
    .with_database(database);
    let workload = Arc::new(McpWorkload::new(&state).expect("MCP workload assembles"));
    let state = state.with_mcp(Arc::clone(&workload));
    state.set_ready(true);

    let shutdown = CancellationToken::new();
    let server = tokio::spawn(run(
        listener,
        build_router(state),
        shutdown.clone().cancelled_owned(),
    ));
    Fixture {
        base_url: format!("http://{address}"),
        workload,
        shutdown,
        server,
    }
}

async fn client(base_url: &str) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let mut headers = HashMap::new();
    headers.insert(
        HeaderName::from_static("x-request-id"),
        HeaderValue::from_static("req_transport_contract"),
    );
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(format!("{base_url}/mcp"))
            .auth_header(TOKEN)
            .custom_headers(headers),
    );
    ().serve(transport).await.expect("initialize succeeds")
}

#[tokio::test]
async fn real_streamable_http_initializes_discovers_and_calls_exact_tools() {
    let fixture = fixture().await;
    let client = client(&fixture.base_url).await;
    let tools = client.list_all_tools().await.expect("tool discovery");
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool.name.as_ref())
            .collect::<Vec<_>>(),
        TOOL_NAMES
    );

    let arguments = serde_json::from_value(json!({
        "project_id": "project-a",
        "message": "Verify real transport",
        "trigger": {"type": "next_session"},
        "session_id": "logical-session-a",
        "idempotency_key": "transport-add"
    }))
    .expect("arguments object");
    let added = client
        .call_tool(CallToolRequestParams::new("remindi_add").with_arguments(arguments))
        .await
        .expect("tool call");
    assert_eq!(added.is_error, Some(false));
    assert_eq!(
        added.structured_content.as_ref().expect("structured")["ok"],
        true
    );

    let listed = client
        .call_tool(CallToolRequestParams::new("remindi_list"))
        .await
        .expect("list call");
    assert_eq!(
        listed.structured_content.as_ref().expect("structured")["data"]["items"][0]["source_session_id"],
        "logical-session-a"
    );

    client.cancel().await.expect("client disconnects");
    fixture.stop().await;
}

#[tokio::test]
async fn auth_host_origin_and_credential_location_fail_safely() {
    let fixture = fixture().await;
    let url = format!("{}/mcp", fixture.base_url);
    let http = reqwest::Client::new();

    let missing = http
        .post(&url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(INITIALIZE)
        .send()
        .await
        .expect("missing auth response");
    assert_eq!(missing.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(missing.text().await.expect("bounded body").len() < 256);

    let wrong = http
        .post(&url)
        .bearer_auth("wrong-token")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(INITIALIZE)
        .send()
        .await
        .expect("wrong auth response");
    assert_eq!(wrong.status(), reqwest::StatusCode::UNAUTHORIZED);

    let bad_host = http
        .post(&url)
        .bearer_auth(TOKEN)
        .header("host", "attacker.invalid")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(INITIALIZE)
        .send()
        .await
        .expect("bad host response");
    assert_eq!(bad_host.status(), reqwest::StatusCode::BAD_REQUEST);

    let bad_origin = http
        .post(&url)
        .bearer_auth(TOKEN)
        .header("origin", "https://attacker.invalid")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(INITIALIZE)
        .send()
        .await
        .expect("bad origin response");
    assert_eq!(bad_origin.status(), reqwest::StatusCode::FORBIDDEN);

    let cookie = http
        .post(&url)
        .bearer_auth(TOKEN)
        .header("cookie", "token=forbidden")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(INITIALIZE)
        .send()
        .await
        .expect("cookie response");
    assert_eq!(cookie.status(), reqwest::StatusCode::BAD_REQUEST);

    let query = http
        .post(format!("{url}?token=forbidden"))
        .bearer_auth(TOKEN)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(INITIALIZE)
        .send()
        .await
        .expect("query response");
    assert_eq!(query.status(), reqwest::StatusCode::BAD_REQUEST);

    let oversized = http
        .post(&url)
        .bearer_auth(TOKEN)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(vec![b'x'; 1024 * 1024 + 1])
        .send()
        .await
        .expect("oversized response");
    assert_eq!(oversized.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);

    let non_exact_path = http
        .post(format!("{url}/other"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .expect("non-exact path response");
    assert_eq!(non_exact_path.status(), reqwest::StatusCode::NOT_FOUND);
    fixture.stop().await;
}

#[tokio::test]
async fn stop_invalidates_sessions_restart_reinitializes_and_health_stays_live() {
    let fixture = fixture().await;
    let old_client = client(&fixture.base_url).await;
    assert!(fixture.workload.is_running());

    fixture.workload.stop().expect("workload stops");
    assert!(!fixture.workload.is_running());
    let http = reqwest::Client::new();
    let stopped = http
        .post(format!("{}/mcp", fixture.base_url))
        .bearer_auth(TOKEN)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(INITIALIZE)
        .send()
        .await
        .expect("stopped response");
    assert_eq!(stopped.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        stopped
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok()),
        Some("1")
    );
    assert!(stopped.text().await.expect("bounded body").len() < 256);
    assert_eq!(
        http.get(format!("{}/health/live", fixture.base_url))
            .send()
            .await
            .expect("health response")
            .status(),
        reqwest::StatusCode::OK
    );
    assert!(old_client.list_all_tools().await.is_err());

    fixture.workload.restart().expect("workload restarts");
    assert!(fixture.workload.is_running());
    let new_client = client(&fixture.base_url).await;
    assert_eq!(
        new_client
            .list_all_tools()
            .await
            .expect("new session")
            .len(),
        8
    );
    new_client.cancel().await.expect("new client disconnects");
    fixture.stop().await;
}

#[tokio::test]
async fn transport_session_header_is_not_the_logical_session_identifier() {
    let fixture = fixture().await;
    let response = reqwest::Client::new()
        .post(format!("{}/mcp", fixture.base_url))
        .bearer_auth(TOKEN)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(INITIALIZE)
        .send()
        .await
        .expect("initialize response");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let transport_session = response
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .expect("transport session")
        .to_owned();
    assert_ne!(transport_session, "logical-session-a");
    fixture.stop().await;
}
