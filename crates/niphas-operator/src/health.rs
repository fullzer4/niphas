use axum::{Router, extract::State, http::StatusCode, routing::get};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Shared health state.
#[derive(Clone)]
pub struct HealthState {
    pub ready: Arc<std::sync::atomic::AtomicBool>,
}

/// Build the health/metrics HTTP server router.
pub fn router(state: HealthState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(state)
}

async fn healthz() -> StatusCode {
    StatusCode::OK
}

async fn readyz(State(state): State<HealthState>) -> StatusCode {
    if state.ready.load(Ordering::Relaxed) {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}
