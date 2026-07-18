use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    sync::Arc,
    time::Duration,
};

use remindi::{
    clock::FixedClock,
    triggers::adapters::{
        AdapterStatus, ConditionAdapter, FileExistsAdapter, FileTarget, HttpHealthAdapter,
        HttpTarget, NetworkPolicy, ObservationWindowAdapter, TcpReachableAdapter, TcpTarget,
        validate_destination,
    },
};
use serde_json::json;
use time::macros::datetime;
use tokio::time::Instant;
use tokio::{io::AsyncReadExt, net::TcpListener};
use tokio_util::sync::CancellationToken;

fn clock() -> Arc<FixedClock> {
    Arc::new(FixedClock::new(datetime!(2026-07-19 06:00 UTC)))
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(2)
}

#[tokio::test]
async fn disabled_and_unknown_aliases_are_safe_unknown_results() {
    let disabled = TcpReachableAdapter::new(false, HashMap::new(), clock()).unwrap();
    let enabled = TcpReachableAdapter::new(true, HashMap::new(), clock()).unwrap();

    let disabled_result = disabled
        .evaluate(
            json!({"target": "anything"}),
            deadline(),
            CancellationToken::new(),
        )
        .await;
    let unknown_result = enabled
        .evaluate(
            json!({"target": "not-configured"}),
            deadline(),
            CancellationToken::new(),
        )
        .await;

    assert_eq!(disabled_result.status, AdapterStatus::Unknown);
    assert_eq!(unknown_result.status, AdapterStatus::Unknown);
    assert_eq!(disabled_result.summary, "Adapter is disabled.");
    assert_eq!(
        unknown_result.summary,
        "Configured target alias was not found."
    );
}

#[tokio::test]
async fn item_parameters_reject_arbitrary_targets_paths_and_unknown_fields() {
    let tcp = TcpReachableAdapter::new(true, HashMap::new(), clock()).unwrap();
    let file = FileExistsAdapter::new(true, vec![], HashMap::new(), clock()).unwrap();
    let http = HttpHealthAdapter::new(true, HashMap::new(), clock()).unwrap();

    for result in [
        tcp.evaluate(
            json!({"host": "127.0.0.1", "port": 22}),
            deadline(),
            CancellationToken::new(),
        )
        .await,
        file.evaluate(
            json!({"path": "/etc/passwd"}),
            deadline(),
            CancellationToken::new(),
        )
        .await,
        http.evaluate(
            json!({"url": "https://example.com", "command": "id"}),
            deadline(),
            CancellationToken::new(),
        )
        .await,
        tcp.evaluate(
            json!({"target": "127.0.0.1:22"}),
            deadline(),
            CancellationToken::new(),
        )
        .await,
        file.evaluate(
            json!({"path_alias": "/etc/passwd"}),
            deadline(),
            CancellationToken::new(),
        )
        .await,
        http.evaluate(
            json!({"target": "https://example.com/health"}),
            deadline(),
            CancellationToken::new(),
        )
        .await,
    ] {
        assert_eq!(result.status, AdapterStatus::Error);
        assert_eq!(result.summary, "Adapter parameters are invalid.");
    }
}

#[test]
fn default_network_policy_blocks_non_public_destinations() {
    let blocked = [
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3)),
        IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1)),
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
        IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)),
        IpAddr::V4(Ipv4Addr::new(100, 100, 100, 200)),
        IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1)),
        IpAddr::V6(Ipv6Addr::LOCALHOST),
        IpAddr::V6("fc00::1".parse().unwrap()),
        IpAddr::V6("fe80::1".parse().unwrap()),
        IpAddr::V6("ff02::1".parse().unwrap()),
        IpAddr::V6("::ffff:127.0.0.1".parse().unwrap()),
    ];

    for address in blocked {
        assert!(validate_destination(address, NetworkPolicy::public_only()).is_err());
    }
}

#[test]
fn malformed_or_insecure_alias_configuration_is_rejected() {
    assert!(
        HttpTarget::new(
            "http://example.com/health",
            vec![200],
            1024,
            None,
            false,
            NetworkPolicy::public_only()
        )
        .is_err()
    );
    assert!(
        HttpTarget::new(
            "https://user:secret@example.com/health",
            vec![200],
            1024,
            None,
            false,
            NetworkPolicy::public_only()
        )
        .is_err()
    );
    assert!(
        HttpTarget::new(
            "https://example.com/health",
            vec![],
            1024,
            None,
            false,
            NetworkPolicy::public_only()
        )
        .is_err()
    );
    assert!(TcpTarget::new("", 80, NetworkPolicy::public_only()).is_err());
    assert!(TcpTarget::new("example.com", 0, NetworkPolicy::public_only()).is_err());
    assert!(FileTarget::new("relative/path").is_err());
}

#[tokio::test]
async fn deadline_and_cancellation_preempt_evaluation() {
    let observation = ObservationWindowAdapter::enabled(clock());
    let cancelled = CancellationToken::new();
    cancelled.cancel();

    let cancelled_result = observation
        .evaluate(
            json!({"window_end": "2026-07-19T07:00:00Z"}),
            deadline(),
            cancelled,
        )
        .await;
    let timeout_result = observation
        .evaluate(
            json!({"window_end": "2026-07-19T07:00:00Z"}),
            Instant::now() - Duration::from_millis(1),
            CancellationToken::new(),
        )
        .await;

    assert_eq!(cancelled_result.status, AdapterStatus::Unknown);
    assert_eq!(
        cancelled_result.summary,
        "Adapter evaluation was cancelled."
    );
    assert_eq!(timeout_result.status, AdapterStatus::Unknown);
    assert_eq!(timeout_result.summary, "Adapter evaluation timed out.");
}

#[cfg(unix)]
#[tokio::test]
async fn file_alias_rejects_symlink_escape_and_traversal() {
    use std::{os::unix::fs::symlink, time::SystemTime};

    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("remindi-root-{nonce}"));
    let outside = std::env::temp_dir().join(format!("remindi-outside-{nonce}"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret"), b"secret").unwrap();
    symlink(&outside, root.join("escape")).unwrap();

    let escaped = FileExistsAdapter::new(
        true,
        vec![root.clone()],
        HashMap::from([(
            "escaped".to_owned(),
            FileTarget::new(root.join("escape/secret")).unwrap(),
        )]),
        clock(),
    );
    let traversed = FileTarget::new(root.join("../not-allowed"));

    assert!(escaped.is_err());
    assert!(traversed.is_err());
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(outside).unwrap();
}

#[test]
fn summaries_and_config_debug_output_do_not_expose_secrets() {
    let target = HttpTarget::new(
        "https://example.com/health?token=secret",
        vec![200],
        1024,
        Some("application/json".to_owned()),
        false,
        NetworkPolicy::public_only(),
    )
    .unwrap();

    let debug = format!("{target:?}");
    assert!(!debug.contains("token=secret"));
    assert!(!debug.contains("/health"));
}

#[tokio::test]
async fn http_health_re_resolves_and_never_disables_tls_validation() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut tls_client_hello = [0_u8; 5];
            socket.read_exact(&mut tls_client_hello).await.unwrap();
            assert_eq!(tls_client_hello[0], 22);
        }
    });
    let target = HttpTarget::new(
        &format!("https://localhost:{port}/health"),
        vec![200],
        1024,
        None,
        false,
        NetworkPolicy::allow_private_for_admin(),
    )
    .unwrap();
    let adapter =
        HttpHealthAdapter::new(true, HashMap::from([("local".to_owned(), target)]), clock())
            .unwrap();

    for _ in 0..2 {
        let result = adapter
            .evaluate(
                json!({"target": "local"}),
                deadline(),
                CancellationToken::new(),
            )
            .await;
        assert_eq!(result.status, AdapterStatus::Error);
        assert_eq!(
            result.summary,
            "Configured target health check failed safely."
        );
    }
    accepted.await.unwrap();
}

#[tokio::test]
async fn http_health_total_deadline_cancels_a_stalled_tls_handshake() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = tokio::spawn(async move {
        let (_socket, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
    });
    let target = HttpTarget::new(
        &format!("https://localhost:{port}/health"),
        vec![200],
        1024,
        None,
        false,
        NetworkPolicy::allow_private_for_admin(),
    )
    .unwrap();
    let adapter =
        HttpHealthAdapter::new(true, HashMap::from([("local".to_owned(), target)]), clock())
            .unwrap();

    let result = adapter
        .evaluate(
            json!({"target": "local"}),
            Instant::now() + Duration::from_millis(25),
            CancellationToken::new(),
        )
        .await;

    assert_eq!(result.status, AdapterStatus::Unknown);
    assert_eq!(result.summary, "Adapter evaluation timed out.");
    accepted.abort();
}
