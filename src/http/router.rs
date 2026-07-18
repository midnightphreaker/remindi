use axum::{
    Router,
    extract::Extension,
    response::IntoResponse,
    routing::{any, get},
};
use tower_http::limit::RequestBodyLimitLayer;

use crate::{
    app::AppState,
    error::AppError,
    http::{
        api, health,
        middleware::{self, RequestId},
    },
};

/// Builds the always-on health and API error shell on the single listener.
pub fn build_router(state: AppState) -> Router {
    let web_api = state
        .web_api()
        .cloned()
        .map(api::router)
        .unwrap_or_default();
    let api = Router::new().nest("/v1", web_api).fallback(api_not_found);
    let mut router = Router::new()
        .route("/health/live", get(health::live))
        .route("/health/ready", get(health::ready))
        .with_state(state.clone())
        .nest("/api", api);
    if let Some(workload) = state.mcp_shared() {
        let mcp = Router::new()
            .route(
                "/mcp",
                any(move |request| {
                    let workload = workload.clone();
                    async move { workload.handle(request).await }
                }),
            )
            .layer(RequestBodyLimitLayer::new(1024 * 1024));
        router = router.merge(mcp);
    }

    middleware::apply(router, state)
}

async fn api_not_found(Extension(request_id): Extension<RequestId>) -> impl IntoResponse {
    AppError::not_found(request_id)
}
