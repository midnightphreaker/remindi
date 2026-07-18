use std::{fs, sync::Arc};

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use remindi::{
    app::AppState,
    clock::{SystemClock, UuidV7Generator},
    config::BootstrapConfig,
    http::router::build_router,
    webui::{AssetOverrides, WebUiAssets, router},
};
use tower::ServiceExt;

#[tokio::test]
async fn embedded_ui_serves_semantic_dependency_free_application() {
    let assets = Arc::new(WebUiAssets::embedded("Remindi"));
    let response = router(assets)
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/html; charset=utf-8"
    );

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(body.to_vec()).unwrap();
    for required in [
        r#"<html lang="en">"#,
        r#"class="skip-link""#,
        r#"<main id="main-content""#,
        r#"<dialog id="login-dialog""#,
        r#"aria-live="polite""#,
        r#"<script type="module" src="assets/app.js"></script>"#,
        r#"id="adapters-view""#,
        r#"id="settings-view""#,
        r#"id="workloads-view""#,
        r#"id="backups-view""#,
        r#"id="audit-view""#,
        r#"<dialog id="restore-dialog""#,
        r#"<input id="restore-password" name="password" type="password""#,
        r#"<input id="restore-confirmation" name="confirmation""#,
    ] {
        assert!(html.contains(required), "missing {required}");
    }
    for forbidden in ["https://", "http://", "react", "vue", "cdn."] {
        assert!(
            !html.to_ascii_lowercase().contains(forbidden),
            "unexpected external/frontend dependency {forbidden}"
        );
    }
}

#[tokio::test]
async fn assets_cover_all_flows_accessibility_and_mobile_states() {
    let assets = Arc::new(WebUiAssets::embedded("Remindi"));
    let app = router(assets);
    let css = response_text(app.clone(), "/assets/app.css").await;
    let js = response_text(app, "/assets/app.js").await;

    assert!(js.contains(
        r#"const API_ROOT = new URL("api/v1", document.baseURI).pathname.replace(/\/$/, "")"#
    ));
    for route in [
        "/session",
        "/auth/login",
        "/auth/logout",
        "/remindi",
        "/remindi/check",
        "/complete",
        "/snooze",
        "/cancel",
        "/history",
        "/settings",
        "/adapters",
        "/workloads",
        "/admin-events",
    ] {
        assert!(js.contains(route), "missing API flow {route}");
    }
    for behavior in [
        "crypto.randomUUID",
        "expected_version",
        "VERSION_CONFLICT",
        "Intl.DateTimeFormat",
        "restoreFocus",
        "aria-invalid",
        "URLSearchParams",
        "data-setting-form",
        "data-adapter-form",
        "data-workload-action",
        "window.confirm",
    ] {
        assert!(js.contains(behavior), "missing behavior {behavior}");
    }
    for behavior in [
        ":focus-visible",
        "prefers-reduced-motion",
        "@media (max-width: 759px)",
        "content: attr(data-label)",
        "overflow-wrap: anywhere",
    ] {
        assert!(css.contains(behavior), "missing CSS behavior {behavior}");
    }
}

#[test]
fn browser_relative_urls_support_root_and_prefixed_mounts() {
    let html = include_str!("../../src/webui/static/index.html");
    for asset in [
        "assets/favicon",
        "assets/app.css",
        "assets/custom.css",
        "assets/app.js",
        "assets/logo",
    ] {
        assert!(
            html.contains(&format!(r#""{asset}""#)),
            "missing relative asset reference {asset}"
        );
        assert_eq!(
            reqwest::Url::parse("https://mcp.phrk.org/")
                .unwrap()
                .join(asset)
                .unwrap()
                .path(),
            format!("/{asset}")
        );
        assert_eq!(
            reqwest::Url::parse("https://mcp.phrk.org/remindi-ui/")
                .unwrap()
                .join(asset)
                .unwrap()
                .path(),
            format!("/remindi-ui/{asset}")
        );
    }

    for (document, expected) in [
        ("https://mcp.phrk.org/", "/api/v1"),
        ("https://mcp.phrk.org/remindi-ui/", "/remindi-ui/api/v1"),
    ] {
        assert_eq!(
            reqwest::Url::parse(document)
                .unwrap()
                .join("api/v1")
                .unwrap()
                .path(),
            expected
        );
    }
}

#[tokio::test]
async fn embedded_ui_payloads_track_the_strict_web_api_contract() {
    let assets = Arc::new(WebUiAssets::embedded("Remindi"));
    let js = response_text(router(assets), "/assets/app.js").await;

    for required in [
        r#"field("message", "Message""#,
        r#"field("instructions", "Instructions"#,
        r#"field("project_id", "Project""#,
        r#"selectField("lifecycle_event", "Lifecycle event""#,
        r#"message: values.message"#,
        r#"priority: values.priority"#,
        r#"trigger: { type: "at_time""#,
        r#"lifecycle_event: values.lifecycle_event"#,
        r#"kind === "check" ? JSON.stringify(body) : mutationBody(body)"#,
        r#"reference_uri: values.reference_uri"#,
        r#"snooze_until: new Date(values.snooze_until).toISOString()"#,
        r#"query.set("states", states)"#,
        r#"state.selected.message"#,
        r#"state.selected.instructions"#,
        r#"state.selected.snooze_until"#,
        r#"`operation-${name}`"#,
    ] {
        assert!(
            js.contains(required),
            "missing strict Web API mapping {required}"
        );
    }

    for legacy in [
        r#"title: values.title"#,
        r#"priority: Number(values.priority)"#,
        r#"type: "time""#,
        r#"notes: values.notes"#,
        r#"type: "manual_verification""#,
        r#"references: [values.reference]"#,
        r#"until: new Date(values.until)"#,
        r#"<label for="${name}">"#,
    ] {
        assert!(
            !js.contains(legacy),
            "legacy mock-only mapping remains: {legacy}"
        );
    }
}

#[tokio::test]
async fn backup_assets_cover_verified_inventory_and_guarded_restore() {
    let assets = Arc::new(WebUiAssets::embedded("Remindi"));
    let js = response_text(router(assets), "/assets/app.js").await;

    for required in [
        r#"api("/backups")"#,
        r#"api("/backups", { method: "POST" })"#,
        r#"api("/backups/upload", { method: "POST", body: data })"#,
        r#"`/backups/${encodeURIComponent(id)}/verify`"#,
        r#"${API_ROOT}/backups/${encodeURIComponent(backup.id)}/download"#,
        r#"api("/auth/reauthenticate""#,
        r#"`/backups/${encodeURIComponent(backupId)}/restore`"#,
        r#"elements.restoreConfirmation.value !== "RESTORE REMINDI""#,
        r#"body: JSON.stringify({ confirmation: "RESTORE REMINDI" })"#,
        r#"elements.restorePassword.value = """#,
        r#"backup.status === "ready" ? "" : "disabled""#,
    ] {
        assert!(
            js.contains(required),
            "missing backup/restore flow {required}"
        );
    }

    for forbidden in [
        r#"localStorage.setItem("restore"#,
        r#"sessionStorage.setItem("restore"#,
        r#"sessionStorage.setItem("password"#,
    ] {
        assert!(
            !js.contains(forbidden),
            "restore credential persisted by browser: {forbidden}"
        );
    }
}

#[tokio::test]
async fn title_is_escaped_and_unknown_asset_is_normal_404() {
    let assets = Arc::new(WebUiAssets::embedded(r#"<script>alert("x")</script>"#));
    let app = router(assets);
    let html = response_text(app.clone(), "/").await;
    assert!(html.contains("&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt;"));
    assert!(!html.contains(r#"<script>alert("x")</script>"#));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/assets/unknown.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(!response.headers().contains_key(header::WWW_AUTHENTICATE));
}

#[tokio::test]
async fn primary_router_mounts_enabled_ui_with_browser_security_headers() {
    let config = Arc::new(
        BootstrapConfig::from_pairs([
            ("REMINDI_OWNER_ID", "owner-a"),
            ("REMINDI_MCP_TOKEN", "test-token"),
            ("REMINDI_WEBUI_ENABLE", "true"),
            ("REMINDI_WEBUI_AUTH", "false"),
        ])
        .unwrap(),
    );
    let state = AppState::new(config, Arc::new(SystemClock), Arc::new(UuidV7Generator))
        .with_webui_assets(Arc::new(WebUiAssets::embedded("Remindi")));
    let response = build_router(state)
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .is_some_and(|value| value.to_str().unwrap().contains("script-src 'self'"))
    );
    assert_eq!(response.headers()["x-frame-options"], "DENY");
    assert!(!response.headers().contains_key(header::WWW_AUTHENTICATE));
}

#[test]
fn blank_overrides_keep_defaults_and_valid_custom_css_is_immutable() {
    let directory = tempfile::tempdir().unwrap();
    let css_path = directory.path().join("custom.css");
    fs::write(&css_path, b":root { --accent: #ffffff; }").unwrap();

    let loaded = WebUiAssets::load(
        "Remindi",
        AssetOverrides {
            custom_css: Some(css_path.clone()),
            ..AssetOverrides::default()
        },
    )
    .unwrap();
    assert_eq!(loaded.custom_css(), b":root { --accent: #ffffff; }");

    fs::write(css_path, b"changed").unwrap();
    assert_eq!(loaded.custom_css(), b":root { --accent: #ffffff; }");
}

async fn response_text(app: axum::Router, uri: &str) -> String {
    let response = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(body.to_vec()).unwrap()
}
