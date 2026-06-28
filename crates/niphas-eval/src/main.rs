use niphas_core::config::NiphasConfig;
use niphas_eval::evaluator::Evaluator;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::info;

#[cfg(feature = "jemalloc")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    niphas_core::telemetry::init_tracing("niphas-eval");
    info!("starting niphas-eval");

    let config = NiphasConfig::load_or_default(None);
    let port = config.health_port;

    let evaluator = Arc::new(Evaluator::new(config)?);
    info!("evaluator initialized");

    let app = niphas_eval::app(evaluator);

    let addr = format!("0.0.0.0:{port}");
    let listener = TcpListener::bind(&addr).await?;
    info!(addr = %addr, "listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    info!("shutdown complete");
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm = signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");

    tokio::select! {
        _ = tokio::signal::ctrl_c() => info!("received SIGINT"),
        _ = sigterm.recv() => info!("received SIGTERM"),
    }
}
