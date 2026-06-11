#[cfg(feature = "jemalloc")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _telemetry = niphas_core::telemetry::init_tracing("niphas-mesh");
    tracing::info!("starting niphas-mesh");
    Ok(())
}
