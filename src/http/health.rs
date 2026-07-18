use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

use crate::app::AppState;

#[derive(Serialize)]
struct HealthBody {
    status: &'static str,
}

/// Returns a minimally revealing process-liveness response.
pub async fn live() -> Json<impl Serialize> {
    Json(HealthBody { status: "ok" })
}

/// Reports whether implemented startup checks have completed.
pub async fn ready(State(state): State<AppState>) -> Response {
    if state.is_ready() {
        (StatusCode::OK, Json(HealthBody { status: "ready" })).into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HealthBody { status: "starting" }),
        )
            .into_response()
    }
}
