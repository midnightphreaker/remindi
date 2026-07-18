use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime},
};

use remindi::{
    clock::FixedClock,
    triggers::adapters::{
        AdapterRegistry, AdapterStatus, ConditionAdapter, FileExistsAdapter, FileTarget,
        NetworkPolicy, ObservationWindowAdapter, TcpReachableAdapter, TcpTarget,
    },
};
use serde_json::json;
use time::macros::datetime;
use tokio::{net::TcpListener, time::Instant};
use tokio_util::sync::CancellationToken;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(2)
}

#[test]
fn registry_contains_exactly_the_four_version_one_adapters() {
    let registry = AdapterRegistry::disabled(Arc::new(FixedClock::new(datetime!(
        2026-07-19 06:00 UTC
    ))));

    assert_eq!(
        registry.names(),
        [
            "file_exists",
            "http_health",
            "observation_window_ended",
            "tcp_reachable"
        ]
    );
}

#[tokio::test]
async fn observation_window_compares_against_the_injected_clock() {
    let adapter = ObservationWindowAdapter::enabled(Arc::new(FixedClock::new(datetime!(
        2026-07-19 06:00 UTC
    ))));

    let ended = adapter
        .evaluate(
            json!({"window_end": "2026-07-19T05:59:59Z"}),
            deadline(),
            CancellationToken::new(),
        )
        .await;
    let future = adapter
        .evaluate(
            json!({"window_end": "2026-07-19T06:00:01Z"}),
            deadline(),
            CancellationToken::new(),
        )
        .await;

    assert_eq!(ended.status, AdapterStatus::Satisfied);
    assert_eq!(future.status, AdapterStatus::Unsatisfied);
    assert_eq!(ended.summary, "Observation window ended.");
    assert_eq!(ended.metadata.adapter_version, "1.0.0");
}

#[tokio::test]
async fn tcp_reachable_connects_without_sending_bytes() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let accepted = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut byte = [0_u8; 1];
        socket.readable().await.unwrap();
        assert_eq!(socket.try_read(&mut byte).unwrap_or(0), 0);
    });
    let adapter = TcpReachableAdapter::new(
        true,
        HashMap::from([(
            "local-test".to_owned(),
            TcpTarget::new(
                "localhost",
                address.port(),
                NetworkPolicy::allow_private_for_admin(),
            )
            .unwrap(),
        )]),
        Arc::new(FixedClock::new(datetime!(2026-07-19 06:00 UTC))),
    )
    .unwrap();

    let result = adapter
        .evaluate(
            json!({"target": "local-test"}),
            deadline(),
            CancellationToken::new(),
        )
        .await;

    assert_eq!(result.status, AdapterStatus::Satisfied);
    assert_eq!(
        result.summary,
        "Configured target accepted a TCP connection."
    );
    accepted.await.unwrap();
}

#[tokio::test]
async fn file_exists_uses_configured_alias_metadata_only() {
    let root = unique_temp_dir();
    let path = root.join("marker");
    std::fs::write(&path, b"secret content that must not be returned").unwrap();
    let adapter = FileExistsAdapter::new(
        true,
        vec![root.clone()],
        HashMap::from([("marker".to_owned(), FileTarget::new(path).unwrap())]),
        Arc::new(FixedClock::new(datetime!(2026-07-19 06:00 UTC))),
    )
    .unwrap();

    let result = adapter
        .evaluate(
            json!({"path_alias": "marker"}),
            deadline(),
            CancellationToken::new(),
        )
        .await;

    assert_eq!(result.status, AdapterStatus::Satisfied);
    assert_eq!(result.summary, "Configured path exists.");
    assert!(!result.summary.contains("secret"));
    std::fs::remove_dir_all(root).unwrap();
}

fn unique_temp_dir() -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("remindi-adapters-{nonce}"));
    std::fs::create_dir_all(&path).unwrap();
    path
}
