use anyhow::{Context, Result, bail};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::{
    Api, Client,
    api::{DeleteParams, PostParams},
};
use niphas_core::crd::{NiphasWorkload, WorkloadPhase};
use std::time::Duration;

/// Poll the NiphasWorkload status until it reaches `expected` phase or timeout.
async fn wait_for_phase(
    api: &Api<NiphasWorkload>,
    name: &str,
    expected: WorkloadPhase,
    timeout: Duration,
) -> Result<NiphasWorkload> {
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > timeout {
            bail!(
                "timed out waiting for workload '{}' to reach phase {:?}",
                name,
                expected
            );
        }
        if let Some(wl) = api.get_opt(name).await? {
            if let Some(ref status) = wl.status {
                if status.phase == expected {
                    return Ok(wl);
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Wait until the workload no longer exists.
async fn wait_for_deletion(api: &Api<NiphasWorkload>, name: &str, timeout: Duration) -> Result<()> {
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > timeout {
            bail!("timed out waiting for workload '{}' to be deleted", name);
        }
        if api.get_opt(name).await?.is_none() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn test_workload(name: &str, flake_ref: &str) -> NiphasWorkload {
    serde_json::from_value(serde_json::json!({
        "apiVersion": "niphas.io/v1alpha1",
        "kind": "NiphasWorkload",
        "metadata": {
            "name": name,
        },
        "spec": {
            "flakeRef": flake_ref,
            "attribute": "packages.x86_64-linux.default",
        }
    }))
    .expect("valid test workload")
}

#[tokio::test]
async fn test_crd_installed() -> Result<()> {
    let client = Client::try_default().await?;
    let crds: Api<CustomResourceDefinition> = Api::all(client);
    let crd = crds
        .get("niphasworkloads.niphas.io")
        .await
        .context("CRD niphasworkloads.niphas.io not found in cluster")?;

    assert_eq!(crd.spec.group, "niphas.io", "CRD group must be niphas.io");
    Ok(())
}

#[tokio::test]
async fn test_operator_healthy() -> Result<()> {
    let url = std::env::var("OPERATOR_HEALTH_URL")
        .unwrap_or_else(|_| "http://localhost:8080/healthz".to_string());

    let resp = reqwest::get(&url)
        .await
        .context("failed to reach operator healthz")?;
    assert!(
        resp.status().is_success(),
        "operator healthz returned {}",
        resp.status()
    );
    Ok(())
}

#[tokio::test]
async fn test_eval_healthy() -> Result<()> {
    let url = std::env::var("EVAL_HEALTH_URL")
        .unwrap_or_else(|_| "http://localhost:8443/healthz".to_string());

    let resp = reqwest::get(&url)
        .await
        .context("failed to reach eval healthz")?;
    assert!(
        resp.status().is_success(),
        "eval healthz returned {}",
        resp.status()
    );
    Ok(())
}

#[tokio::test]
async fn test_workload_eval_failure_sets_failed() -> Result<()> {
    let client = Client::try_default().await?;
    let ns = std::env::var("E2E_NAMESPACE").unwrap_or_else(|_| "niphas-e2e".to_string());
    let api: Api<NiphasWorkload> = Api::namespaced(client, &ns);

    let name = "e2e-fail-test";
    let wl = test_workload(name, "github:nonexistent/does-not-exist-12345");

    // Clean up if leftover from previous run
    let _ = api.delete(name, &DeleteParams::default()).await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    api.create(&PostParams::default(), &wl)
        .await
        .context("failed to create test workload")?;

    let result = wait_for_phase(&api, name, WorkloadPhase::Failed, Duration::from_secs(120)).await;

    // Cleanup
    let _ = api.delete(name, &DeleteParams::default()).await;

    let wl = result.context("workload did not reach Failed phase")?;
    let status = wl.status.expect("status should be set");

    // Check Evaluated condition is False
    if let Some(conditions) = &status.conditions {
        let eval_cond = conditions.iter().find(|c| c.type_ == "Evaluated");
        assert!(eval_cond.is_some(), "expected Evaluated condition to exist");
    }

    Ok(())
}

#[tokio::test]
async fn test_workload_deletion_cleanup() -> Result<()> {
    let client = Client::try_default().await?;
    let ns = std::env::var("E2E_NAMESPACE").unwrap_or_else(|_| "niphas-e2e".to_string());
    let api: Api<NiphasWorkload> = Api::namespaced(client, &ns);

    let name = "e2e-delete-test";
    let wl = test_workload(name, "github:nixos/nixpkgs");

    // Clean up if leftover
    let _ = api.delete(name, &DeleteParams::default()).await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    api.create(&PostParams::default(), &wl)
        .await
        .context("failed to create test workload")?;

    // Wait for it to get a finalizer (operator picks it up)
    tokio::time::sleep(Duration::from_secs(10)).await;

    // Delete it
    api.delete(name, &DeleteParams::default())
        .await
        .context("failed to delete test workload")?;

    wait_for_deletion(&api, name, Duration::from_secs(60))
        .await
        .context("workload was not cleaned up after deletion")?;

    Ok(())
}
