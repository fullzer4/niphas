pub mod allowlist;
pub mod error;
pub mod evaluator;
pub mod handlers;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use axum::routing::{get, post};
use evaluator::Evaluator;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceBuilder;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

const MAX_REQUEST_BODY: usize = 1024 * 1024; // 1MB

pub fn app(evaluator: Arc<Evaluator>) -> Router {
    let timeout = evaluator.config().eval_timeout + Duration::from_secs(5);

    let middleware = ServiceBuilder::new()
        .layer(CatchPanicLayer::new())
        .layer(TraceLayer::new_for_http())
        .layer(TimeoutLayer::with_status_code(
            StatusCode::GATEWAY_TIMEOUT,
            timeout,
        ));

    Router::new()
        .route("/evaluate", post(handlers::evaluate))
        .route("/healthz", get(handlers::healthz))
        .route("/readyz", get(handlers::readyz))
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY))
        .layer(middleware)
        .with_state(evaluator)
}
