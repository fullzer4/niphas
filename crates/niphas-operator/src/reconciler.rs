use crate::context::Context;
use crate::error::OperatorError;
use crate::eval::{self, EvalResult, EvalResultExt};
use crate::resources;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::ObjectReference;
use kube::api::{Api, Patch, PatchParams, ResourceExt};
use kube::runtime::controller::Action;
use kube_runtime::events::{Event, EventType};
use niphas_core::crd::{ConditionStatus, NiphasWorkload, NiphasWorkloadStatus, WorkloadPhase};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info, warn};

const FINALIZER: &str = "niphas.io/workload-cleanup";

/// Main reconciliation function called by the kube-rs Controller.
pub async fn reconcile(
    workload: Arc<NiphasWorkload>,
    ctx: Arc<Context>,
) -> Result<Action, OperatorError> {
    let name = workload.name_any();
    let ns = workload.namespace().unwrap_or_default();
    let generation = workload.metadata.generation.unwrap_or(0);

    debug!(%name, %ns, generation, "reconciling");

    if workload.metadata.deletion_timestamp.is_some() {
        return handle_deletion(&workload, &ctx, &name, &ns).await;
    }

    let has_finalizer = workload
        .metadata
        .finalizers
        .as_ref()
        .map(|f| f.iter().any(|s| s == FINALIZER))
        .unwrap_or(false);
    if !has_finalizer {
        add_finalizer(&ctx, &workload, &ns).await?;
        return Ok(Action::requeue(Duration::from_secs(0)));
    }

    let status = workload.status.as_ref();
    let observed = status.and_then(|s| s.observed_generation).unwrap_or(0);

    if observed >= generation && status.map(|s| s.phase) == Some(WorkloadPhase::Running) {
        return Ok(Action::requeue(Duration::from_secs(300)));
    }

    let eval_result = if needs_eval(&workload, observed, generation) {
        set_phase(&ctx, &workload, &ns, WorkloadPhase::Evaluating).await?;
        publish_event(
            &ctx,
            &workload,
            EventType::Normal,
            "EvalStarted",
            "Starting flake evaluation",
        )
        .await;

        let timeout = ctx.config.eval_timeout;
        let eval_url = &ctx.config.eval_webhook_url;

        match eval::call_eval_webhook(&ctx.http, eval_url, &workload, timeout).await {
            Ok(result) => {
                publish_event(
                    &ctx,
                    &workload,
                    EventType::Normal,
                    "EvalSucceeded",
                    &format!("Resolved store path: {}", result.store_path),
                )
                .await;
                result
            }
            Err(e) => {
                let (reason, msg) = match &e {
                    OperatorError::EvalTimeout(secs) => (
                        "EvalTimeout".to_string(),
                        format!("Evaluation timed out after {secs}s"),
                    ),
                    OperatorError::EvalFailed { code, message } => (code.clone(), message.clone()),
                    other => ("EvalFailed".to_string(), other.to_string()),
                };

                publish_event(&ctx, &workload, EventType::Warning, &reason, &msg).await;
                set_failed(&ctx, &workload, &ns, &reason, &msg, generation).await?;
                return Ok(Action::requeue(Duration::from_secs(30)));
            }
        }
    } else {
        // Use cached eval from status
        match EvalResult::from_status(status.unwrap_or(&NiphasWorkloadStatus::default())) {
            Some(cached) => cached,
            None => {
                // No cached result, need full eval
                set_phase(&ctx, &workload, &ns, WorkloadPhase::Evaluating).await?;
                let timeout = ctx.config.eval_timeout;
                let eval_url = &ctx.config.eval_webhook_url;

                match eval::call_eval_webhook(&ctx.http, eval_url, &workload, timeout).await {
                    Ok(result) => result,
                    Err(e) => {
                        set_failed(
                            &ctx,
                            &workload,
                            &ns,
                            "EvalFailed",
                            &e.to_string(),
                            generation,
                        )
                        .await?;
                        return Ok(Action::requeue(Duration::from_secs(30)));
                    }
                }
            }
        }
    };

    set_phase(&ctx, &workload, &ns, WorkloadPhase::Provisioning).await?;

    if let Err(e) = resources::apply_child_resources(&ctx, &workload, &eval_result).await {
        error!(error = %e, "failed to apply child resources");
        set_failed(
            &ctx,
            &workload,
            &ns,
            "ProvisionFailed",
            &e.to_string(),
            generation,
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(10)));
    }

    publish_event(
        &ctx,
        &workload,
        EventType::Normal,
        "Provisioned",
        "Child resources applied",
    )
    .await;

    let ready = count_ready_replicas(&ctx, &workload, &ns).await?;
    let desired = workload.spec.replicas.unwrap_or(1);

    let phase = if ready >= desired {
        WorkloadPhase::Running
    } else {
        WorkloadPhase::Provisioning
    };

    if phase == WorkloadPhase::Running {
        publish_event(
            &ctx,
            &workload,
            EventType::Normal,
            "Available",
            "All replicas ready",
        )
        .await;
    }

    update_status(&ctx, &workload, &ns, phase, &eval_result, ready, generation).await?;

    let interval = if ready < desired {
        Duration::from_secs(5)
    } else {
        Duration::from_secs(300)
    };
    Ok(Action::requeue(interval))
}

/// Error policy: determine retry behavior on reconcile error.
pub fn error_policy(
    _workload: Arc<NiphasWorkload>,
    error: &OperatorError,
    _ctx: Arc<Context>,
) -> Action {
    warn!(error = %error, "reconcile error");
    if error.is_transient() {
        Action::requeue(Duration::from_secs(10))
    } else {
        Action::requeue(Duration::from_secs(60))
    }
}

fn needs_eval(workload: &NiphasWorkload, observed: i64, generation: i64) -> bool {
    if observed < generation {
        return true;
    }
    // No store path yet (first reconciliation after crash)
    workload
        .status
        .as_ref()
        .and_then(|s| s.store_path.as_ref())
        .is_none()
}

async fn add_finalizer(
    ctx: &Context,
    workload: &NiphasWorkload,
    ns: &str,
) -> Result<(), OperatorError> {
    let api: Api<NiphasWorkload> = Api::namespaced(ctx.client.clone(), ns);
    // Append our finalizer without clobbering existing ones from other controllers.
    let mut finalizers = workload.metadata.finalizers.clone().unwrap_or_default();
    if !finalizers.iter().any(|f| f == FINALIZER) {
        finalizers.push(FINALIZER.to_string());
    }
    let patch = json!({
        "metadata": {
            "finalizers": finalizers
        }
    });
    api.patch(
        &workload.name_any(),
        &PatchParams::default(),
        &Patch::Merge(&patch),
    )
    .await?;
    Ok(())
}

async fn remove_finalizer(
    ctx: &Context,
    workload: &NiphasWorkload,
    ns: &str,
) -> Result<(), OperatorError> {
    let api: Api<NiphasWorkload> = Api::namespaced(ctx.client.clone(), ns);
    // Remove only our finalizer, preserving others.
    let finalizers: Vec<String> = workload
        .metadata
        .finalizers
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|f| f.as_str() != FINALIZER)
        .cloned()
        .collect();
    let patch = if finalizers.is_empty() {
        json!({ "metadata": { "finalizers": null } })
    } else {
        json!({ "metadata": { "finalizers": finalizers } })
    };
    api.patch(
        &workload.name_any(),
        &PatchParams::default(),
        &Patch::Merge(&patch),
    )
    .await?;
    Ok(())
}

async fn handle_deletion(
    workload: &NiphasWorkload,
    ctx: &Context,
    name: &str,
    ns: &str,
) -> Result<Action, OperatorError> {
    info!(%name, %ns, "handling deletion");
    publish_event(
        ctx,
        workload,
        EventType::Normal,
        "Deleting",
        &format!("Cleaning up workload {name}"),
    )
    .await;

    // Child resources are cleaned by ownerReferences.
    // Remove finalizer to allow K8s to complete deletion.
    remove_finalizer(ctx, workload, ns).await?;
    Ok(Action::await_change())
}

async fn set_phase(
    ctx: &Context,
    workload: &NiphasWorkload,
    ns: &str,
    phase: WorkloadPhase,
) -> Result<(), OperatorError> {
    let api: Api<NiphasWorkload> = Api::namespaced(ctx.client.clone(), ns);
    let patch = json!({
        "status": {
            "phase": phase
        }
    });
    api.patch_status(
        &workload.name_any(),
        &PatchParams::default(),
        &Patch::Merge(&patch),
    )
    .await?;
    Ok(())
}

async fn set_failed(
    ctx: &Context,
    workload: &NiphasWorkload,
    ns: &str,
    reason: &str,
    message: &str,
    generation: i64,
) -> Result<(), OperatorError> {
    let api: Api<NiphasWorkload> = Api::namespaced(ctx.client.clone(), ns);
    let now = iso_now();
    let patch = json!({
        "status": {
            "phase": WorkloadPhase::Failed,
            "observedGeneration": generation,
            "conditions": [{
                "type": "Evaluated",
                "status": ConditionStatus::False,
                "reason": reason,
                "message": message,
                "lastTransitionTime": now,
                "observedGeneration": generation,
            }]
        }
    });
    api.patch_status(
        &workload.name_any(),
        &PatchParams::default(),
        &Patch::Merge(&patch),
    )
    .await?;
    Ok(())
}

async fn update_status(
    ctx: &Context,
    workload: &NiphasWorkload,
    ns: &str,
    phase: WorkloadPhase,
    eval_result: &EvalResult,
    ready_replicas: i32,
    generation: i64,
) -> Result<(), OperatorError> {
    let api: Api<NiphasWorkload> = Api::namespaced(ctx.client.clone(), ns);
    let now = iso_now();

    let progressing_status = if phase == WorkloadPhase::Provisioning {
        ConditionStatus::True
    } else {
        ConditionStatus::False
    };
    let progressing_reason = if phase == WorkloadPhase::Running {
        "NewReplicaSetAvailable"
    } else {
        "ReplicaSetUpdated"
    };

    let mut conditions = vec![
        json!({
            "type": "Evaluated",
            "status": ConditionStatus::True,
            "reason": "EvalSucceeded",
            "message": format!("Resolved {}", eval_result.store_path),
            "lastTransitionTime": now,
            "observedGeneration": generation,
        }),
        json!({
            "type": "Progressing",
            "status": progressing_status,
            "reason": progressing_reason,
            "message": format!("{ready_replicas} of {} replicas ready", workload.spec.replicas.unwrap_or(1)),
            "lastTransitionTime": now,
            "observedGeneration": generation,
        }),
    ];

    if phase == WorkloadPhase::Running {
        conditions.push(json!({
            "type": "Available",
            "status": ConditionStatus::True,
            "reason": "ReplicasReady",
            "message": format!("All {ready_replicas} replicas are ready"),
            "lastTransitionTime": now,
            "observedGeneration": generation,
        }));
    }

    let patch = json!({
        "status": {
            "phase": phase,
            "observedGeneration": generation,
            "storePath": eval_result.store_path,
            "resolvedCommand": eval_result.resolved_command(),
            "closurePaths": eval_result.closure_paths,
            "readyReplicas": ready_replicas,
            "conditions": conditions,
        }
    });

    api.patch_status(
        &workload.name_any(),
        &PatchParams::default(),
        &Patch::Merge(&patch),
    )
    .await?;
    Ok(())
}

async fn count_ready_replicas(
    ctx: &Context,
    workload: &NiphasWorkload,
    ns: &str,
) -> Result<i32, OperatorError> {
    let deploys: Api<Deployment> = Api::namespaced(ctx.client.clone(), ns);
    let name = workload.name_any();

    match deploys.get_opt(&name).await? {
        Some(deploy) => {
            let ready = deploy
                .status
                .as_ref()
                .and_then(|s| s.ready_replicas)
                .unwrap_or(0);
            Ok(ready)
        }
        None => Ok(0),
    }
}

async fn publish_event(
    ctx: &Context,
    workload: &NiphasWorkload,
    type_: EventType,
    reason: &str,
    note: &str,
) {
    let recorder = ctx.recorder.clone();
    let obj_ref = ObjectReference {
        api_version: Some("niphas.io/v1alpha1".into()),
        kind: Some("NiphasWorkload".into()),
        name: workload.metadata.name.clone(),
        namespace: workload.metadata.namespace.clone(),
        uid: workload.metadata.uid.clone(),
        ..Default::default()
    };
    let event = Event {
        type_,
        reason: reason.into(),
        note: Some(note.into()),
        action: reason.into(),
        secondary: None,
    };
    if let Err(e) = recorder.publish(&event, &obj_ref).await {
        warn!(error = %e, reason, "failed to publish event");
    }
}

fn iso_now() -> String {
    let now = time::OffsetDateTime::now_utc();
    now.format(&time::format_description::well_known::Rfc3339)
        .expect("RFC 3339 formatting cannot fail")
}
