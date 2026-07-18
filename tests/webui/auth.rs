use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use remindi::{
    auth::web_session::{LoginError, SessionError, WebMode, WebSessionManager},
    clock::{SystemClock, UuidV7Generator},
    config::BootstrapConfig,
    db::DatabaseManager,
    http::api::{WebApiState, router},
    remindi::RemindiService,
};
use time::{Duration, OffsetDateTime};
use tower::ServiceExt;
use uuid::Uuid;

fn config(extra: &[(&str, &str)]) -> Arc<BootstrapConfig> {
    let mut values = vec![
        ("REMINDI_OWNER_ID", "owner-a"),
        ("REMINDI_MCP_TOKEN", "mcp-token-not-for-web"),
        ("REMINDI_WEBUI_USERNAME", "owner"),
        ("REMINDI_WEBUI_PASSWORD", "correct horse battery staple"),
    ];
    values.extend_from_slice(extra);
    Arc::new(BootstrapConfig::from_pairs(values).expect("valid config"))
}

async fn state(config: Arc<BootstrapConfig>) -> WebApiState {
    let directory = std::env::temp_dir().join(format!("remindi-web-auth-{}", Uuid::now_v7()));
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
        Arc::new(SystemClock),
        Arc::new(UuidV7Generator),
    ));
    WebApiState::new(
        WebSessionManager::from_config(&config).expect("sessions"),
        service,
    )
}

#[test]
fn all_three_modes_are_explicit() {
    let authenticated = WebSessionManager::from_config(&config(&[])).expect("sessions");
    assert_eq!(authenticated.mode(), WebMode::Authenticated);
    let unauthenticated =
        WebSessionManager::from_config(&config(&[("REMINDI_WEBUI_AUTH", "false")]))
            .expect("sessions");
    assert_eq!(unauthenticated.mode(), WebMode::Unauthenticated);
    let disabled = WebSessionManager::from_config(&config(&[("REMINDI_WEBUI_ENABLE", "false")]))
        .expect("sessions");
    assert_eq!(disabled.mode(), WebMode::Disabled);
}

#[test]
fn login_cookie_expiry_logout_restart_and_rate_limit_are_enforced() {
    let config = config(&[
        ("REMINDI_WEBUI_SESSION_TTL_SECONDS", "60"),
        ("REMINDI_WEBUI_COOKIE_SECURE", "true"),
    ]);
    let sessions = WebSessionManager::from_config(&config).expect("sessions");
    let now = OffsetDateTime::now_utc();
    let nonce = sessions.issue_pre_session_token(now).expect("nonce");
    let login = sessions
        .login("owner", "correct horse battery staple", &nonce, now)
        .expect("login");
    let cookie = login.set_cookie.to_str().expect("cookie");
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Strict"));
    assert!(cookie.contains("Secure"));
    assert!(!cookie.contains("correct horse"));

    let mut headers = http::HeaderMap::new();
    headers.insert(
        header::COOKIE,
        cookie.split(';').next().unwrap().parse().unwrap(),
    );
    assert!(sessions.authenticate(&headers, now).is_ok());
    let cleared = sessions.logout(&headers);
    assert!(cleared.to_str().unwrap().contains("Max-Age=0"));
    assert!(cleared.to_str().unwrap().contains("Secure"));
    assert_eq!(
        sessions.authenticate(&headers, now),
        Err(SessionError::Unauthenticated)
    );

    let nonce = sessions.issue_pre_session_token(now).expect("nonce");
    let login = sessions
        .login("owner", "correct horse battery staple", &nonce, now)
        .expect("second login");
    headers.insert(
        header::COOKIE,
        login
            .set_cookie
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .parse()
            .unwrap(),
    );
    assert_eq!(
        sessions.authenticate(&headers, now + Duration::seconds(61)),
        Err(SessionError::Expired)
    );

    let restarted = WebSessionManager::from_config(&config).expect("restart sessions");
    assert_eq!(
        restarted.authenticate(&headers, now),
        Err(SessionError::Unauthenticated)
    );

    for _ in 0..5 {
        let nonce = restarted.issue_pre_session_token(now).expect("nonce");
        assert!(matches!(
            restarted.login("owner", "wrong", &nonce, now),
            Err(LoginError::InvalidCredentials)
        ));
    }
    let nonce = restarted.issue_pre_session_token(now).expect("nonce");
    assert!(matches!(
        restarted.login("owner", "wrong", &nonce, now),
        Err(LoginError::RateLimited)
    ));
}

#[test]
fn password_reauthentication_updates_only_a_live_authenticated_session() {
    let sessions = WebSessionManager::from_config(&config(&[])).expect("sessions");
    let now = OffsetDateTime::now_utc();
    let nonce = sessions.issue_pre_session_token(now).expect("nonce");
    let login = sessions
        .login("owner", "correct horse battery staple", &nonce, now)
        .expect("login");
    let mut headers = http::HeaderMap::new();
    headers.insert(
        header::COOKIE,
        login
            .set_cookie
            .to_str()
            .expect("cookie")
            .split(';')
            .next()
            .expect("cookie pair")
            .parse()
            .expect("header"),
    );

    assert!(matches!(
        sessions.reauthenticate(&headers, "wrong", now + Duration::minutes(1)),
        Err(LoginError::InvalidCredentials)
    ));
    let refreshed = sessions
        .reauthenticate(
            &headers,
            "correct horse battery staple",
            now + Duration::minutes(2),
        )
        .expect("reauthenticated");
    assert_eq!(
        refreshed.session.reauthenticated_at,
        Some(now + Duration::minutes(2))
    );
}

#[tokio::test]
async fn login_is_application_json_without_basic_challenge_and_security_headers_are_set() {
    let state = state(config(&[])).await;
    let app = router(state);
    let session_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(session_response.status(), StatusCode::OK);
    assert!(
        session_response
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .is_none()
    );
    assert_eq!(
        session_response
            .headers()
            .get("x-frame-options")
            .and_then(|value| value.to_str().ok()),
        Some("DENY")
    );

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/login")
                .header(header::HOST, "remindi.local")
                .header(header::ORIGIN, "https://evil.example")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"username":"owner","password":"wrong"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(response.headers().get(header::WWW_AUTHENTICATE).is_none());
}

#[tokio::test]
async fn valid_http_login_establishes_and_logout_revokes_the_cookie_session() {
    let app = router(state(config(&[])).await);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let session: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let csrf = session["data"]["csrf_token"].as_str().unwrap();

    let login = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/login")
                .header(header::HOST, "remindi.local")
                .header(header::ORIGIN, "http://remindi.local")
                .header("x-csrf-token", csrf)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"username":"owner","password":"correct horse battery staple"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    assert!(login.headers().get(header::WWW_AUTHENTICATE).is_none());
    let cookie = login
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let bytes = login.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let session_csrf = body["data"]["csrf_token"].as_str().unwrap();

    let logout = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/logout")
                .header(header::HOST, "remindi.local")
                .header(header::ORIGIN, "http://remindi.local")
                .header(header::COOKIE, &cookie)
                .header("x-csrf-token", session_csrf)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::OK);
    assert!(
        logout
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );

    let after = app
        .oneshot(
            Request::builder()
                .uri("/session")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = after.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["data"]["authenticated"], false);
}

#[tokio::test]
async fn disabled_mode_has_no_routes() {
    let state = state(config(&[("REMINDI_WEBUI_ENABLE", "false")])).await;
    let response = router(state)
        .oneshot(
            Request::builder()
                .uri("/session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
