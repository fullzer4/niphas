pub mod allowlist;
pub mod error;
pub mod evaluator;
pub mod handlers;

use axum::Router;
use axum::routing::{get, post};
use evaluator::Evaluator;
use std::sync::Arc;

/// Build the Axum router for the eval service.
pub fn app(evaluator: Arc<Evaluator>) -> Router {
    Router::new()
        .route("/evaluate", post(handlers::evaluate))
        .route("/healthz", get(handlers::healthz))
        .route("/readyz", get(handlers::readyz))
        .with_state(evaluator)
}
