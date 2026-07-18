use axum::{Router, extract::Extension, response::IntoResponse, routing::get};

use crate::{
    app::AppState,
    error::AppError,
    http::{
        health,
        middleware::{self, RequestId},
    },
};

/// Builds the always-on health and API error shell on the single listener.
pub fn build_router(state: AppState) -> Router {
    let api = Router::new().fallback(api_not_found);
    let router = Router::new()
        .route("/health/live", get(health::live))
        .route("/health/ready", get(health::ready))
        .nest("/api/v1", api)
        .with_state(state.clone());

    middleware::apply(router, state)
}

async fn api_not_found(Extension(request_id): Extension<RequestId>) -> impl IntoResponse {
    AppError::not_found(request_id)
}
