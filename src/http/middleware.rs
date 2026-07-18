use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{
        HeaderValue,
        header::{AUTHORIZATION, COOKIE},
    },
    middleware::{self, Next},
    response::Response,
};
use tower_http::{
    sensitive_headers::SetSensitiveRequestHeadersLayer,
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::{Instrument, Level, Span};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use crate::{app::AppState, config::BootstrapConfig, error::TracingError};

/// Validated request identifier carried through handlers and responses.
#[derive(Clone, Debug)]
pub struct RequestId(String);

impl RequestId {
    /// Returns the request identifier as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the wrapper and returns the identifier.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

/// Installs structured JSON logging on stderr.
///
/// Request spans intentionally omit headers, bodies, credentials, and content.
///
/// # Errors
///
/// Returns [`TracingError`] for an invalid filter or an existing subscriber.
pub fn init_json_tracing(config: &BootstrapConfig) -> Result<(), TracingError> {
    let filter = EnvFilter::try_new(config.log_level())?;
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().json())
        .try_init()?;
    Ok(())
}

/// Applies privacy-preserving request IDs and tracing to the control plane.
pub fn apply(router: Router, state: AppState) -> Router {
    let trace = TraceLayer::new_for_http()
        .make_span_with(|request: &Request<Body>| {
            let request_id = request
                .extensions()
                .get::<RequestId>()
                .map_or("", RequestId::as_str);
            tracing::info_span!(
                "http.request",
                event = "http_request",
                request_id,
                method = %request.method(),
                route = request.uri().path(),
                actor = tracing::field::Empty,
                remindi_id = tracing::field::Empty,
                outcome = tracing::field::Empty,
                error_code = tracing::field::Empty,
            )
        })
        .on_response(DefaultOnResponse::new().level(Level::INFO));

    router
        .layer(trace)
        .layer(SetSensitiveRequestHeadersLayer::new([
            AUTHORIZATION,
            COOKIE,
        ]))
        .layer(middleware::from_fn_with_state(state, request_id))
}

async fn request_id(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(valid_request_id)
        .map_or_else(
            || format!("req_{}", state.ids().next_id().simple()),
            str::to_owned,
        );

    let header = HeaderValue::from_str(&request_id)
        .unwrap_or_else(|_| HeaderValue::from_static("req_invalid"));
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));
    request.headers_mut().insert("x-request-id", header.clone());

    let mut response = next.run(request).instrument(Span::current()).await;
    response.headers_mut().insert("x-request-id", header);
    response
}

fn valid_request_id(value: &HeaderValue) -> Option<&str> {
    let value = value.to_str().ok()?;
    (value.len() <= 128
        && !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')))
    .then_some(value)
}
