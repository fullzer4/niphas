mod allowlist;
mod error;
mod evaluator;
mod handlers;

use axum::routing::{get, post};
use axum::Router;
use evaluator::Evaluator;
use niphas_core::config::NiphasConfig;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::info;

#[cfg(feature = "jemalloc")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _telemetry = niphas_core::telemetry::init_tracing("niphas-eval");
    info!("starting niphas-eval");

    let config = NiphasConfig::load_or_default(None);
    let port = config.health_port;

    let evaluator = Arc::new(Evaluator::new(config)?);
    info!("evaluator initialized");

    let app = Router::new()
        .route("/evaluate", post(handlers::evaluate))
        .route("/healthz", get(handlers::healthz))
        .route("/readyz", get(handlers::readyz))
        .with_state(evaluator);

    let addr = format!("0.0.0.0:{port}");
    let listener = TcpListener::bind(&addr).await?;
    info!(addr = %addr, "listening");

    axum::serve(listener, app).await?;

    Ok(())
}
