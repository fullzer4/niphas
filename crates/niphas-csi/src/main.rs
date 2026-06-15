use niphas_core::config::NiphasConfig;
use niphas_csi::cache::NarCache;
use niphas_csi::csi;
use niphas_csi::identity::IdentityService;
use niphas_csi::node::NodeService;
use std::sync::Arc;
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;
use tracing::info;

#[cfg(feature = "jemalloc")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

const CSI_SOCKET_PATH: &str = "/csi/csi.sock";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _telemetry = niphas_core::telemetry::init_tracing("niphas-csi");
    info!("starting niphas-csi");

    let config = NiphasConfig::load_or_default(None);

    let node_id = std::env::var("NODE_NAME").unwrap_or_else(|_| {
        hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".into())
    });
    info!(node_id, "node identity");

    let nar_cache = Arc::new(NarCache::new(&config)?);
    nar_cache.cleanup_temp_dirs();

    let socket_path = std::env::var("CSI_ENDPOINT").unwrap_or_else(|_| CSI_SOCKET_PATH.into());
    let _ = std::fs::remove_file(&socket_path);
    if let Some(parent) = std::path::Path::new(&socket_path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(&socket_path)?;
    let stream = UnixListenerStream::new(listener);

    info!(socket = %socket_path, "listening on UDS");

    Server::builder()
        .add_service(csi::identity_server::IdentityServer::new(IdentityService))
        .add_service(csi::node_server::NodeServer::new(NodeService::new(
            node_id, nar_cache,
        )))
        .serve_with_incoming(stream)
        .await?;

    Ok(())
}
