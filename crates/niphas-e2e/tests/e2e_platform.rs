use anyhow::{Context, Result, bail};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::{
    Api, Client,
    api::{DeleteParams, ListParams, Patch, PatchParams, PostParams},
};
use niphas_core::crd::{ConditionStatus, NiphasWorkload, WorkloadPhase};
use std::time::Duration;

const FINALIZER: &str = "niphas.io/workload-cleanup";

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
            let current = api.get_opt(name).await?.map(|w| {
                w.status
                    .as_ref()
                    .map(|s| format!("phase={:?}", s.phase))
                    .unwrap_or_else(|| "no status".to_string())
            });
            bail!(
                "timed out waiting for workload '{}' to reach phase {:?} (current: {:?})",
                name,
                expected,
                current
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

/// Poll the NiphasWorkload status until a condition with `type_` has `status`,
/// or timeout. This is more reliable than waiting for a phase, since phases
/// can flip-flop during reconciliation retries.
async fn wait_for_condition(
    api: &Api<NiphasWorkload>,
    name: &str,
    condition_type: &str,
    expected_status: ConditionStatus,
    timeout: Duration,
) -> Result<NiphasWorkload> {
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > timeout {
            bail!(
                "timed out waiting for workload '{}' condition '{}' = {:?}",
                name,
                condition_type,
                expected_status
            );
        }
        if let Some(wl) = api.get_opt(name).await? {
            if let Some(ref status) = wl.status {
                if let Some(conditions) = &status.conditions {
                    if conditions
                        .iter()
                        .any(|c| c.type_ == condition_type && c.status == expected_status)
                    {
                        return Ok(wl);
                    }
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

/// Wait until the workload has the niphas finalizer.
async fn wait_for_finalizer(
    api: &Api<NiphasWorkload>,
    name: &str,
    timeout: Duration,
) -> Result<NiphasWorkload> {
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > timeout {
            bail!("timed out waiting for workload '{}' to get finalizer", name);
        }
        if let Some(wl) = api.get_opt(name).await? {
            let has_finalizer = wl
                .metadata
                .finalizers
                .as_ref()
                .is_some_and(|f| f.iter().any(|s| s == FINALIZER));
            if has_finalizer {
                return Ok(wl);
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Clean up a workload, ignoring errors if it doesn't exist.
async fn cleanup_workload(api: &Api<NiphasWorkload>, name: &str) {
    let _ = api.delete(name, &DeleteParams::default()).await;
    let _ = wait_for_deletion(api, name, Duration::from_secs(30)).await;
}

fn e2e_namespace() -> String {
    std::env::var("E2E_NAMESPACE").unwrap_or_else(|_| "niphas-e2e".to_string())
}

fn test_workload(name: &str, flake_ref: &str, attribute: &str) -> NiphasWorkload {
    serde_json::from_value(serde_json::json!({
        "apiVersion": "niphas.io/v1alpha1",
        "kind": "NiphasWorkload",
        "metadata": {
            "name": name,
        },
        "spec": {
            "flakeRef": flake_ref,
            "attribute": attribute,
            "command": ["/bin/sleep", "3600"],
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
    let ns = e2e_namespace();
    let api: Api<NiphasWorkload> = Api::namespaced(client, &ns);

    let name = "e2e-fail-test";
    let wl = test_workload(
        name,
        "github:nonexistent/does-not-exist-12345",
        "packages.x86_64-linux.default",
    );

    cleanup_workload(&api, name).await;

    api.create(&PostParams::default(), &wl)
        .await
        .context("failed to create test workload")?;

    // Wait for the Evaluated condition to become False (eval failure).
    // We check the condition rather than the phase because the reconciler
    // retries eval failures, flip-flopping the phase between Evaluating and Failed.
    let result = wait_for_condition(
        &api,
        name,
        "Evaluated",
        ConditionStatus::False,
        Duration::from_secs(120),
    )
    .await;

    cleanup_workload(&api, name).await;

    let wl = result.context("Evaluated condition did not become False")?;
    let status = wl.status.expect("status should be set");

    let conditions = status
        .conditions
        .as_ref()
        .expect("workload should have conditions after eval failure");
    let eval_cond = conditions
        .iter()
        .find(|c| c.type_ == "Evaluated")
        .expect("Evaluated condition should exist");
    assert_eq!(
        eval_cond.status,
        ConditionStatus::False,
        "Evaluated condition should be False on failure"
    );
    assert_eq!(eval_cond.reason, "EvalFailed");

    Ok(())
}

#[tokio::test]
async fn test_workload_full_pipeline() -> Result<()> {
    let client = Client::try_default().await?;
    let ns = e2e_namespace();
    let api: Api<NiphasWorkload> = Api::namespaced(client.clone(), &ns);

    let name = "e2e-pipeline-test";
    let wl = test_workload(
        name,
        "github:NixOS/nixpkgs",
        "legacyPackages.x86_64-linux.hello",
    );

    cleanup_workload(&api, name).await;

    api.create(&PostParams::default(), &wl)
        .await
        .context("failed to create test workload")?;

    // Wait for Provisioning phase (eval succeeded, child resources applied).
    // Reaching Provisioning proves the full eval pipeline:
    //   Pending → Evaluating → [nix eval + closure resolution] → Provisioning
    let wl = wait_for_phase(
        &api,
        name,
        WorkloadPhase::Provisioning,
        Duration::from_secs(180),
    )
    .await
    .context("workload did not reach Provisioning phase")?;

    let status = wl.status.expect("status should be set");

    // Verify store_path starts with /nix/store/
    let store_path = status
        .store_path
        .as_ref()
        .expect("store_path should be set after successful eval");
    assert!(
        store_path.starts_with("/nix/store/"),
        "store_path should start with /nix/store/, got: {store_path}"
    );

    // Verify closure_paths has more than 1 path (hello + glibc + etc)
    let closure_paths = status
        .closure_paths
        .as_ref()
        .expect("closure_paths should be set");
    assert!(
        closure_paths.len() > 1,
        "closure should contain multiple paths (hello + deps), got: {closure_paths:?}"
    );

    // Verify Evaluated condition is True
    let conditions = status
        .conditions
        .as_ref()
        .expect("should have conditions after eval");
    let eval_cond = conditions
        .iter()
        .find(|c| c.type_ == "Evaluated")
        .expect("Evaluated condition should exist");
    assert_eq!(
        eval_cond.status,
        ConditionStatus::True,
        "Evaluated condition should be True"
    );
    assert_eq!(eval_cond.reason, "EvalSucceeded");

    // Verify operator created a child Deployment
    let deployments: Api<Deployment> = Api::namespaced(client, &ns);
    let lp = ListParams::default().labels(&format!("niphas.io/workload={name}"));
    let dep_list = deployments
        .list(&lp)
        .await
        .context("failed to list child deployments")?;
    assert_eq!(
        dep_list.items.len(),
        1,
        "operator should have created exactly one child Deployment"
    );

    cleanup_workload(&api, name).await;

    Ok(())
}

#[tokio::test]
async fn test_workload_deletion_cleanup() -> Result<()> {
    let client = Client::try_default().await?;
    let ns = e2e_namespace();
    let api: Api<NiphasWorkload> = Api::namespaced(client, &ns);

    let name = "e2e-delete-test";
    let wl = test_workload(
        name,
        "github:NixOS/nixpkgs",
        "legacyPackages.x86_64-linux.hello",
    );

    cleanup_workload(&api, name).await;

    api.create(&PostParams::default(), &wl)
        .await
        .context("failed to create test workload")?;

    // Wait for the operator to add the finalizer
    wait_for_finalizer(&api, name, Duration::from_secs(30))
        .await
        .context("operator did not add finalizer")?;

    // Delete it
    api.delete(name, &DeleteParams::default())
        .await
        .context("failed to delete test workload")?;

    wait_for_deletion(&api, name, Duration::from_secs(60))
        .await
        .context("workload was not cleaned up after deletion")?;

    Ok(())
}

// ─────────────────────────────────────────────────────────────
// New e2e tests: reconciler behavior
// ─────────────────────────────────────────────────────────────

/// Verify that the operator adds the finalizer to a new workload.
#[tokio::test]
async fn test_workload_gets_finalizer() -> Result<()> {
    let client = Client::try_default().await?;
    let ns = e2e_namespace();
    let api: Api<NiphasWorkload> = Api::namespaced(client, &ns);

    let name = "e2e-finalizer-test";
    let wl = test_workload(
        name,
        "github:NixOS/nixpkgs",
        "legacyPackages.x86_64-linux.hello",
    );

    cleanup_workload(&api, name).await;

    api.create(&PostParams::default(), &wl)
        .await
        .context("failed to create test workload")?;

    let wl = wait_for_finalizer(&api, name, Duration::from_secs(30))
        .await
        .context("operator did not add finalizer")?;

    let finalizers = wl.metadata.finalizers.as_ref().unwrap();
    assert!(
        finalizers.contains(&FINALIZER.to_string()),
        "expected finalizer '{}', got: {:?}",
        FINALIZER,
        finalizers
    );

    cleanup_workload(&api, name).await;
    Ok(())
}

/// Verify that after eval succeeds, observedGeneration matches generation
/// and the workload has a valid store_path.
#[tokio::test]
async fn test_workload_eval_produces_valid_status() -> Result<()> {
    let client = Client::try_default().await?;
    let ns = e2e_namespace();
    let api: Api<NiphasWorkload> = Api::namespaced(client, &ns);

    let name = "e2e-stable-test";
    let wl = test_workload(
        name,
        "github:NixOS/nixpkgs",
        "legacyPackages.x86_64-linux.hello",
    );

    cleanup_workload(&api, name).await;

    api.create(&PostParams::default(), &wl)
        .await
        .context("failed to create test workload")?;

    // Wait for Evaluated=True condition (proves eval succeeded)
    let wl = wait_for_condition(
        &api,
        name,
        "Evaluated",
        ConditionStatus::True,
        Duration::from_secs(180),
    )
    .await
    .context("Evaluated condition did not become True")?;

    let status = wl.status.as_ref().unwrap();
    let generation = wl.metadata.generation.unwrap_or(0);
    let observed = status.observed_generation.unwrap_or(-1);
    assert_eq!(
        observed, generation,
        "observedGeneration ({observed}) should match generation ({generation})"
    );

    // Verify resolved_command is set (mainProgram resolved)
    assert!(
        status.resolved_command.is_some(),
        "resolved_command should be set after eval"
    );

    cleanup_workload(&api, name).await;
    Ok(())
}

/// Verify Evaluated and Progressing conditions are set correctly
/// after eval succeeds and provisioning starts.
#[tokio::test]
async fn test_workload_conditions_after_eval() -> Result<()> {
    let client = Client::try_default().await?;
    let ns = e2e_namespace();
    let api: Api<NiphasWorkload> = Api::namespaced(client, &ns);

    let name = "e2e-conditions-test";
    let wl = test_workload(
        name,
        "github:NixOS/nixpkgs",
        "legacyPackages.x86_64-linux.hello",
    );

    cleanup_workload(&api, name).await;

    api.create(&PostParams::default(), &wl)
        .await
        .context("failed to create test workload")?;

    // Wait for Provisioning (eval succeeded, child resources created)
    let wl = wait_for_phase(
        &api,
        name,
        WorkloadPhase::Provisioning,
        Duration::from_secs(180),
    )
    .await
    .context("workload did not reach Provisioning")?;

    let conditions = wl
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_ref())
        .expect("Provisioning workload should have conditions");

    // Evaluated = True
    let evaluated = conditions
        .iter()
        .find(|c| c.type_ == "Evaluated")
        .expect("Evaluated condition should exist");
    assert_eq!(
        evaluated.status,
        ConditionStatus::True,
        "Evaluated should be True"
    );
    assert_eq!(evaluated.reason, "EvalSucceeded");

    // Progressing = True (still provisioning replicas)
    let progressing = conditions
        .iter()
        .find(|c| c.type_ == "Progressing")
        .expect("Progressing condition should exist");
    assert_eq!(
        progressing.status,
        ConditionStatus::True,
        "Progressing should be True during Provisioning"
    );

    cleanup_workload(&api, name).await;
    Ok(())
}

/// Verify that changing the spec triggers re-evaluation.
#[tokio::test]
async fn test_workload_spec_update_triggers_reeval() -> Result<()> {
    let client = Client::try_default().await?;
    let ns = e2e_namespace();
    let api: Api<NiphasWorkload> = Api::namespaced(client, &ns);

    let name = "e2e-reeval-test";
    let wl = test_workload(
        name,
        "github:NixOS/nixpkgs",
        "legacyPackages.x86_64-linux.hello",
    );

    cleanup_workload(&api, name).await;

    api.create(&PostParams::default(), &wl)
        .await
        .context("failed to create test workload")?;

    // Wait for initial eval to succeed
    wait_for_condition(
        &api,
        name,
        "Evaluated",
        ConditionStatus::True,
        Duration::from_secs(180),
    )
    .await
    .context("initial eval did not succeed")?;

    let generation_before = api.get(name).await?.metadata.generation.unwrap_or(0);

    // Update the spec — this bumps generation and should trigger re-eval
    let patch = serde_json::json!({
        "spec": {
            "args": ["--version"]
        }
    });
    api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .context("failed to patch workload spec")?;

    // Generation should have incremented
    let wl_after_patch = api.get(name).await?;
    let generation_after = wl_after_patch.metadata.generation.unwrap_or(0);
    assert!(
        generation_after > generation_before,
        "generation should increment after spec change ({generation_before} -> {generation_after})"
    );

    // Wait for re-eval to complete — the Evaluated condition should update
    // with the new observedGeneration matching the new generation.
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(180);
    loop {
        if start.elapsed() > timeout {
            bail!("timed out waiting for re-evaluation after spec update");
        }
        if let Some(wl) = api.get_opt(name).await? {
            if let Some(ref status) = wl.status {
                if status.observed_generation == Some(generation_after) {
                    // Re-eval happened for the new generation
                    break;
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    let settled = api.get(name).await?;
    let status = settled.status.as_ref().unwrap();
    assert_eq!(
        status.observed_generation.unwrap_or(0),
        generation_after,
        "observedGeneration should match new generation after re-eval"
    );

    cleanup_workload(&api, name).await;
    Ok(())
}

/// Verify that the workload passes through Evaluating before Provisioning.
#[tokio::test]
async fn test_workload_passes_through_evaluating() -> Result<()> {
    let client = Client::try_default().await?;
    let ns = e2e_namespace();
    let api: Api<NiphasWorkload> = Api::namespaced(client, &ns);

    let name = "e2e-provisioning-test";
    let wl = test_workload(
        name,
        "github:NixOS/nixpkgs",
        "legacyPackages.x86_64-linux.hello",
    );

    cleanup_workload(&api, name).await;

    api.create(&PostParams::default(), &wl)
        .await
        .context("failed to create test workload")?;

    // Poll rapidly to observe the Evaluating → Provisioning transition
    let mut saw_evaluating = false;
    let mut saw_provisioning = false;
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(180);

    loop {
        if start.elapsed() > timeout {
            bail!(
                "timed out (saw_evaluating={saw_evaluating}, saw_provisioning={saw_provisioning})"
            );
        }
        if let Some(wl) = api.get_opt(name).await? {
            if let Some(ref status) = wl.status {
                match status.phase {
                    WorkloadPhase::Evaluating => saw_evaluating = true,
                    WorkloadPhase::Provisioning => {
                        saw_provisioning = true;
                        break;
                    }
                    _ => {}
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    assert!(saw_evaluating, "workload should pass through Evaluating");
    assert!(saw_provisioning, "workload should reach Provisioning");

    cleanup_workload(&api, name).await;
    Ok(())
}
