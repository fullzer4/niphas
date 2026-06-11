mod context;
mod error;
mod eval;
mod health;
mod reconciler;
mod resources;

use context::Context;
use futures::StreamExt;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Pod;
use kube::Client;
use kube::api::Api;
use kube::runtime::Controller;
use kube_leader_election::{LeaseLock, LeaseLockParams, LeaseLockResult};
use niphas_core::config::NiphasConfig;
use niphas_core::crd::NiphasWorkload;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

#[cfg(feature = "jemalloc")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

const LEASE_NAME: &str = "niphas-operator-leader";
const LEASE_TTL: Duration = Duration::from_secs(15);
const LEASE_RENEW_INTERVAL: Duration = Duration::from_secs(5);
/// Max consecutive renewal failures before giving up leadership.
const MAX_RENEW_FAILURES: u32 = 3;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _telemetry = niphas_core::telemetry::init_tracing("niphas-operator");
    info!("starting niphas-operator");

    let config = NiphasConfig::load_or_default(None);
    let health_port = config.health_port;

    let client = Client::try_default().await?;
    info!("connected to kubernetes");

    let ctx = Arc::new(Context::new(client.clone(), config));
    let ready = ctx.ready.clone();
    let shutdown = CancellationToken::new();

    // Start health server (runs regardless of leadership)
    let health_state = health::HealthState {
        ready: ready.clone(),
    };
    let health_router = health::router(health_state);
    let health_listener = TcpListener::bind(format!("0.0.0.0:{health_port}")).await?;
    info!(port = health_port, "health server listening");

    let health_shutdown = shutdown.clone();
    tokio::spawn(async move {
        let serve = axum::serve(health_listener, health_router);
        tokio::select! {
            result = serve => {
                if let Err(e) = result {
                    error!(error = %e, "health server failed");
                }
            }
            _ = health_shutdown.cancelled() => {}
        }
    });

    // Leader election: only the leader runs the controller
    let holder_id = std::env::var("POD_NAME")
        .unwrap_or_else(|_| format!("niphas-operator-{}", std::process::id()));

    let lease_namespace =
        std::env::var("NAMESPACE").unwrap_or_else(|_| "niphas-system".to_string());

    let leadership = LeaseLock::new(
        client.clone(),
        &lease_namespace,
        LeaseLockParams {
            holder_id: holder_id.clone(),
            lease_name: LEASE_NAME.into(),
            lease_ttl: LEASE_TTL,
        },
    );

    info!(holder_id = %holder_id, "starting leader election");

    // Acquire leadership before starting the controller
    loop {
        match leadership.try_acquire_or_renew().await? {
            LeaseLockResult::Acquired(_) => {
                info!("acquired leadership");
                break;
            }
            LeaseLockResult::NotAcquired(lease) => {
                let holder = lease
                    .spec
                    .as_ref()
                    .and_then(|s| s.holder_identity.as_deref())
                    .unwrap_or("unknown");
                info!(leader = %holder, "waiting for leadership");
                tokio::time::sleep(LEASE_RENEW_INTERVAL).await;
            }
        }
    }

    // Spawn lease renewal in the background
    let renew_shutdown = shutdown.clone();
    let ready_for_renewer = ready.clone();
    tokio::spawn(async move {
        let mut consecutive_failures: u32 = 0;

        loop {
            tokio::select! {
                _ = tokio::time::sleep(LEASE_RENEW_INTERVAL) => {}
                _ = renew_shutdown.cancelled() => break,
            }

            match leadership.try_acquire_or_renew().await {
                Ok(LeaseLockResult::Acquired(_)) => {
                    consecutive_failures = 0;
                    tracing::trace!("lease renewed");
                }
                Ok(LeaseLockResult::NotAcquired(_)) => {
                    warn!("lost leadership, initiating shutdown");
                    ready_for_renewer.store(false, Ordering::Relaxed);
                    renew_shutdown.cancel();
                    break;
                }
                Err(e) => {
                    consecutive_failures += 1;
                    warn!(
                        error = %e,
                        attempt = consecutive_failures,
                        max = MAX_RENEW_FAILURES,
                        "lease renewal failed"
                    );
                    if consecutive_failures >= MAX_RENEW_FAILURES {
                        error!("max renewal failures reached, initiating shutdown");
                        ready_for_renewer.store(false, Ordering::Relaxed);
                        renew_shutdown.cancel();
                        break;
                    }
                }
            }
        }
    });

    // Start controller (only runs as leader)
    let workloads: Api<NiphasWorkload> = Api::all(client.clone());
    let deployments: Api<Deployment> = Api::all(client.clone());
    let pods: Api<Pod> = Api::all(client.clone());

    info!("starting controller");
    ready.store(true, Ordering::Relaxed);

    let controller_shutdown = shutdown.clone();
    let controller = Controller::new(workloads, Default::default())
        .owns(deployments, Default::default())
        .owns(pods, Default::default())
        .run(reconciler::reconcile, reconciler::error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok(o) => {
                    tracing::debug!(resource = ?o, "reconciled");
                }
                Err(e) => {
                    error!(error = %e, "reconcile failed");
                }
            }
        });

    tokio::select! {
        _ = controller => {
            warn!("controller stream ended unexpectedly");
        }
        _ = controller_shutdown.cancelled() => {
            info!("shutting down after losing leadership");
        }
    }

    // Graceful shutdown: ready=false lets K8s drain traffic before restart
    ready.store(false, Ordering::Relaxed);
    info!("shutdown complete");

    // _telemetry guard drops here, flushing pending spans/logs
    Ok(())
}
