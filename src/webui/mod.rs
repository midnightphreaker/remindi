//! Embedded, dependency-free WebUI assets and composable static routes.

mod assets;

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};

pub use assets::{AssetError, AssetOverrides, WebUiAssets};

/// Builds the static WebUI route group.
///
/// The primary HTTP router can merge this router after Task 9 has selected the
/// enabled/authenticated WebUI mode. It deliberately owns no authentication or
/// API state.
pub fn router(assets: Arc<WebUiAssets>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/assets/app.css", get(app_css))
        .route("/assets/custom.css", get(custom_css))
        .route("/assets/app.js", get(app_js))
        .route("/assets/logo", get(logo))
        .route("/assets/favicon", get(favicon))
        .with_state(assets)
}

async fn index(State(assets): State<Arc<WebUiAssets>>) -> Response {
    asset_response(
        StatusCode::OK,
        "text/html; charset=utf-8",
        assets.index().to_vec(),
    )
}

async fn app_css(State(assets): State<Arc<WebUiAssets>>) -> Response {
    asset_response(
        StatusCode::OK,
        "text/css; charset=utf-8",
        assets.app_css().to_vec(),
    )
}

async fn custom_css(State(assets): State<Arc<WebUiAssets>>) -> Response {
    asset_response(
        StatusCode::OK,
        "text/css; charset=utf-8",
        assets.custom_css().to_vec(),
    )
}

async fn app_js(State(assets): State<Arc<WebUiAssets>>) -> Response {
    asset_response(
        StatusCode::OK,
        "text/javascript; charset=utf-8",
        assets.app_js().to_vec(),
    )
}

async fn logo(State(assets): State<Arc<WebUiAssets>>) -> Response {
    asset_response(
        StatusCode::OK,
        assets.logo_content_type(),
        assets.logo().to_vec(),
    )
}

async fn favicon(State(assets): State<Arc<WebUiAssets>>) -> Response {
    asset_response(
        StatusCode::OK,
        assets.favicon_content_type(),
        assets.favicon().to_vec(),
    )
}

fn asset_response(status: StatusCode, content_type: &'static str, body: Vec<u8>) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        Body::from(body),
    )
        .into_response()
}
