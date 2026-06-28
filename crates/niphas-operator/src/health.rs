use axum::{Router, extract::State, http::StatusCode, routing::get};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tower::ServiceBuilder;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::trace::TraceLayer;

#[derive(Clone)]
pub struct HealthState {
    pub ready: Arc<std::sync::atomic::AtomicBool>,
}

pub fn router(state: HealthState) -> Router {
    let middleware = ServiceBuilder::new()
        .layer(CatchPanicLayer::new())
        .layer(TraceLayer::new_for_http());

    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .layer(middleware)
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
