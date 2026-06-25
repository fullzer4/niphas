use crate::context::Context;
use crate::error::OperatorError;
use crate::eval;
use crate::resources;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::ObjectReference;
use kube::api::{Api, Patch, PatchParams, ResourceExt};
use kube::runtime::controller::Action;
use kube_runtime::events::{Event, EventType};
use niphas_core::crd::{
    ConditionStatus, NiphasCondition, NiphasWorkload, NiphasWorkloadStatus, WorkloadPhase,
};
use niphas_core::eval::EvalResponse;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info, warn};

const FINALIZER: &str = "niphas.io/workload-cleanup";

pub struct WorkloadState<'a> {
    pub name: String,
    pub namespace: String,
    pub generation: i64,
    pub deletion_pending: bool,
    pub has_finalizer: bool,
    pub existing_finalizers: Vec<String>,
    pub observed_generation: i64,
    pub phase: Option<WorkloadPhase>,
    pub status: Option<&'a NiphasWorkloadStatus>,
    pub desired_replicas: i32,
}

impl<'a> WorkloadState<'a> {
    pub fn from_workload(workload: &'a NiphasWorkload) -> Self {
        let status = workload.status.as_ref();
        let finalizers = workload.metadata.finalizers.clone().unwrap_or_default();
        let has_finalizer = finalizers.iter().any(|s| s == FINALIZER);

        WorkloadState {
            name: workload.name_any(),
            namespace: workload.namespace().unwrap_or_default(),
            generation: workload.metadata.generation.unwrap_or(0),
            deletion_pending: workload.metadata.deletion_timestamp.is_some(),
            has_finalizer,
            existing_finalizers: finalizers,
            observed_generation: status.and_then(|s| s.observed_generation).unwrap_or(0),
            phase: status.map(|s| s.phase),
            status,
            desired_replicas: workload.spec.replicas.unwrap_or(1),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReconcileDecision {
    HandleDeletion,
    AddFinalizer {
        existing_finalizers: Vec<String>,
    },
    UpToDate,
    RunEval,
    Provision {
        eval_result: EvalResponse,
    },
    UpdateStatus {
        eval_result: EvalResponse,
        ready_replicas: i32,
        desired_replicas: i32,
    },
}

enum ExecuteOutcome {
    Done(Action),
    EvalCompleted(EvalResponse),
    Provisioned {
        eval_result: EvalResponse,
        ready_replicas: i32,
    },
}

pub fn decide(state: &WorkloadState) -> ReconcileDecision {
    if state.deletion_pending {
        return ReconcileDecision::HandleDeletion;
    }

    if !state.has_finalizer {
        return ReconcileDecision::AddFinalizer {
            existing_finalizers: state.existing_finalizers.clone(),
        };
    }

    if state.observed_generation >= state.generation && state.phase == Some(WorkloadPhase::Running)
    {
        return ReconcileDecision::UpToDate;
    }

    // Generation changed — must re-evaluate regardless of cache
    if state.observed_generation < state.generation {
        return ReconcileDecision::RunEval;
    }

    // Same generation, not Running — use cached eval or re-evaluate
    match state.status.and_then(eval::eval_result_from_status) {
        Some(cached) => ReconcileDecision::Provision {
            eval_result: cached,
        },
        None => ReconcileDecision::RunEval,
    }
}

fn make_condition(
    type_: &str,
    status: ConditionStatus,
    reason: &str,
    message: &str,
    generation: i64,
) -> NiphasCondition {
    NiphasCondition {
        type_: type_.to_string(),
        status,
        reason: reason.to_string(),
        message: message.to_string(),
        last_transition_time: iso_now(),
        observed_generation: Some(generation),
    }
}

fn upsert_conditions(
    existing: Option<&[NiphasCondition]>,
    new: Vec<NiphasCondition>,
) -> Vec<NiphasCondition> {
    let mut result: Vec<NiphasCondition> = existing.map(|s| s.to_vec()).unwrap_or_default();

    for cond in new {
        if let Some(pos) = result.iter().position(|c| c.type_ == cond.type_) {
            result[pos] = cond;
        } else {
            result.push(cond);
        }
    }

    result
}

fn build_success_conditions(
    eval: &EvalResponse,
    phase: WorkloadPhase,
    ready: i32,
    desired: i32,
    generation: i64,
) -> Vec<NiphasCondition> {
    let (progressing_status, progressing_reason) = if phase == WorkloadPhase::Running {
        (ConditionStatus::False, "NewReplicaSetAvailable")
    } else {
        (ConditionStatus::True, "ReplicaSetUpdated")
    };

    let mut conditions = vec![
        make_condition(
            "Evaluated",
            ConditionStatus::True,
            "EvalSucceeded",
            &format!("Resolved {}", eval.store_path),
            generation,
        ),
        make_condition(
            "Progressing",
            progressing_status,
            progressing_reason,
            &format!("{ready} of {desired} replicas ready"),
            generation,
        ),
    ];

    if phase == WorkloadPhase::Running {
        conditions.push(make_condition(
            "Available",
            ConditionStatus::True,
            "ReplicasReady",
            &format!("All {ready} replicas are ready"),
            generation,
        ));
    }

    conditions
}

fn build_failed_conditions(reason: &str, message: &str, generation: i64) -> Vec<NiphasCondition> {
    vec![make_condition(
        "Evaluated",
        ConditionStatus::False,
        reason,
        message,
        generation,
    )]
}

struct StatusPatch<'a> {
    phase: WorkloadPhase,
    generation: i64,
    eval_result: Option<&'a EvalResponse>,
    ready_replicas: Option<i32>,
    conditions: Vec<NiphasCondition>,
}

async fn patch_status(
    ctx: &Context,
    name: &str,
    ns: &str,
    patch: StatusPatch<'_>,
) -> Result<(), OperatorError> {
    let api: Api<NiphasWorkload> = Api::namespaced(ctx.client.clone(), ns);

    let mut status = serde_json::Map::new();
    status.insert("phase".into(), json!(patch.phase));
    status.insert("observedGeneration".into(), json!(patch.generation));
    status.insert(
        "conditions".into(),
        serde_json::to_value(&patch.conditions)?,
    );

    if let Some(eval) = patch.eval_result {
        status.insert("storePath".into(), json!(eval.store_path));
        status.insert("resolvedCommand".into(), json!(eval.resolved_command()));
        status.insert("closurePaths".into(), json!(eval.closure_paths));
    }

    if let Some(ready) = patch.ready_replicas {
        status.insert("readyReplicas".into(), json!(ready));
    }

    let body = json!({ "status": status });
    api.patch_status(name, &PatchParams::default(), &Patch::Merge(&body))
        .await?;
    Ok(())
}

async fn execute(
    decision: ReconcileDecision,
    workload: &NiphasWorkload,
    ctx: &Context,
    state: &WorkloadState<'_>,
) -> Result<ExecuteOutcome, OperatorError> {
    match decision {
        ReconcileDecision::HandleDeletion => {
            info!(name = %state.name, ns = %state.namespace, "handling deletion");
            publish_event(
                ctx,
                workload,
                EventType::Normal,
                "Deleting",
                &format!("Cleaning up workload {}", state.name),
            )
            .await;
            remove_finalizer(ctx, workload, &state.namespace).await?;
            Ok(ExecuteOutcome::Done(Action::await_change()))
        }

        ReconcileDecision::AddFinalizer {
            mut existing_finalizers,
        } => {
            existing_finalizers.push(FINALIZER.to_string());
            let api: Api<NiphasWorkload> = Api::namespaced(ctx.client.clone(), &state.namespace);
            let patch = json!({ "metadata": { "finalizers": existing_finalizers } });
            api.patch(&state.name, &PatchParams::default(), &Patch::Merge(&patch))
                .await?;
            Ok(ExecuteOutcome::Done(Action::requeue(Duration::from_secs(
                0,
            ))))
        }

        ReconcileDecision::UpToDate => Ok(ExecuteOutcome::Done(Action::requeue(
            Duration::from_secs(300),
        ))),

        ReconcileDecision::RunEval => {
            set_phase(ctx, workload, &state.namespace, WorkloadPhase::Evaluating).await?;
            publish_event(
                ctx,
                workload,
                EventType::Normal,
                "EvalStarted",
                "Starting flake evaluation",
            )
            .await;

            let timeout = ctx.config.eval_timeout;
            let eval_url = &ctx.config.eval_webhook_url;

            match eval::call_eval_webhook(&ctx.http, eval_url, workload, timeout).await {
                Ok(result) => {
                    publish_event(
                        ctx,
                        workload,
                        EventType::Normal,
                        "EvalSucceeded",
                        &format!("Resolved store path: {}", result.store_path),
                    )
                    .await;
                    Ok(ExecuteOutcome::EvalCompleted(result))
                }
                Err(e) => {
                    let (reason, msg) = e.reason_and_message();
                    publish_event(ctx, workload, EventType::Warning, &reason, &msg).await;

                    let conditions = upsert_conditions(
                        state.status.and_then(|s| s.conditions.as_deref()),
                        build_failed_conditions(&reason, &msg, state.generation),
                    );
                    patch_status(
                        ctx,
                        &state.name,
                        &state.namespace,
                        StatusPatch {
                            phase: WorkloadPhase::Failed,
                            generation: state.generation,
                            eval_result: None,
                            ready_replicas: None,
                            conditions,
                        },
                    )
                    .await?;
                    Ok(ExecuteOutcome::Done(Action::requeue(Duration::from_secs(
                        30,
                    ))))
                }
            }
        }

        ReconcileDecision::Provision { eval_result } => {
            set_phase(ctx, workload, &state.namespace, WorkloadPhase::Provisioning).await?;

            if let Err(e) = resources::apply_child_resources(ctx, workload, &eval_result).await {
                error!(error = %e, "failed to apply child resources");
                let conditions = upsert_conditions(
                    state.status.and_then(|s| s.conditions.as_deref()),
                    build_failed_conditions("ProvisionFailed", &e.to_string(), state.generation),
                );
                patch_status(
                    ctx,
                    &state.name,
                    &state.namespace,
                    StatusPatch {
                        phase: WorkloadPhase::Failed,
                        generation: state.generation,
                        eval_result: None,
                        ready_replicas: None,
                        conditions,
                    },
                )
                .await?;
                return Ok(ExecuteOutcome::Done(Action::requeue(Duration::from_secs(
                    10,
                ))));
            }

            publish_event(
                ctx,
                workload,
                EventType::Normal,
                "Provisioned",
                "Child resources applied",
            )
            .await;

            let ready = count_ready_replicas(ctx, workload, &state.namespace).await?;
            Ok(ExecuteOutcome::Provisioned {
                eval_result,
                ready_replicas: ready,
            })
        }

        ReconcileDecision::UpdateStatus {
            eval_result,
            ready_replicas,
            desired_replicas,
        } => {
            let phase = if ready_replicas >= desired_replicas {
                WorkloadPhase::Running
            } else {
                WorkloadPhase::Provisioning
            };

            if phase == WorkloadPhase::Running {
                publish_event(
                    ctx,
                    workload,
                    EventType::Normal,
                    "Available",
                    "All replicas ready",
                )
                .await;
            }

            let conditions = upsert_conditions(
                state.status.and_then(|s| s.conditions.as_deref()),
                build_success_conditions(
                    &eval_result,
                    phase,
                    ready_replicas,
                    desired_replicas,
                    state.generation,
                ),
            );

            patch_status(
                ctx,
                &state.name,
                &state.namespace,
                StatusPatch {
                    phase,
                    generation: state.generation,
                    eval_result: Some(&eval_result),
                    ready_replicas: Some(ready_replicas),
                    conditions,
                },
            )
            .await?;

            let interval = if ready_replicas < desired_replicas {
                Duration::from_secs(5)
            } else {
                Duration::from_secs(300)
            };
            Ok(ExecuteOutcome::Done(Action::requeue(interval)))
        }
    }
}

pub async fn reconcile(
    workload: Arc<NiphasWorkload>,
    ctx: Arc<Context>,
) -> Result<Action, OperatorError> {
    let state = WorkloadState::from_workload(&workload);
    debug!(name = %state.name, ns = %state.namespace, generation = state.generation, "reconciling");

    let mut decision = decide(&state);
    loop {
        match execute(decision, &workload, &ctx, &state).await? {
            ExecuteOutcome::Done(action) => return Ok(action),
            ExecuteOutcome::EvalCompleted(result) => {
                decision = ReconcileDecision::Provision {
                    eval_result: result,
                };
            }
            ExecuteOutcome::Provisioned {
                eval_result,
                ready_replicas,
            } => {
                decision = ReconcileDecision::UpdateStatus {
                    eval_result,
                    ready_replicas,
                    desired_replicas: state.desired_replicas,
                };
            }
        }
    }
}

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

async fn remove_finalizer(
    ctx: &Context,
    workload: &NiphasWorkload,
    ns: &str,
) -> Result<(), OperatorError> {
    let api: Api<NiphasWorkload> = Api::namespaced(ctx.client.clone(), ns);
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

async fn set_phase(
    ctx: &Context,
    workload: &NiphasWorkload,
    ns: &str,
    phase: WorkloadPhase,
) -> Result<(), OperatorError> {
    let api: Api<NiphasWorkload> = Api::namespaced(ctx.client.clone(), ns);
    let patch = json!({ "status": { "phase": phase } });
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
        Some(deploy) => Ok(deploy
            .status
            .as_ref()
            .and_then(|s| s.ready_replicas)
            .unwrap_or(0)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use niphas_core::crd::{NiphasWorkloadSpec, NiphasWorkloadStatus, WorkloadPhase};

    fn base_workload() -> NiphasWorkload {
        NiphasWorkload {
            metadata: kube::api::ObjectMeta {
                name: Some("test-app".to_string()),
                namespace: Some("default".to_string()),
                generation: Some(1),
                uid: Some("uid-1234".to_string()),
                ..Default::default()
            },
            spec: NiphasWorkloadSpec {
                flake_ref: "github:test/app".into(),
                attribute: "packages.x86_64-linux.default".into(),
                revision: None,
                replicas: Some(2),
                binary_cache: None,
                command: None,
                args: None,
                env: None,
                ports: None,
                resources: None,
                liveness_probe: None,
                readiness_probe: None,
                startup_probe: None,
                node_selector: None,
                tolerations: None,
                affinity: None,
                service: None,
                ingress: None,
                extra_volumes: None,
                extra_volume_mounts: None,
                architectures: None,
            },
            status: None,
        }
    }

    fn with_finalizer(workload: &mut NiphasWorkload) {
        workload.metadata.finalizers = Some(vec![FINALIZER.to_string()]);
    }

    fn running_status(generation: i64) -> NiphasWorkloadStatus {
        NiphasWorkloadStatus {
            observed_generation: Some(generation),
            phase: WorkloadPhase::Running,
            store_path: Some("/nix/store/abc123-myapp-1.0".to_string()),
            resolved_command: Some("/nix/store/abc123-myapp-1.0/bin/myapp".to_string()),
            closure_paths: Some(vec!["/nix/store/abc123-myapp-1.0".to_string()]),
            ready_replicas: Some(2),
            conditions: None,
            last_eval: None,
            architectures: None,
        }
    }

    #[test]
    fn decide_deletion_takes_priority() {
        let state = WorkloadState {
            name: "test-app".into(),
            namespace: "default".into(),
            generation: 1,
            deletion_pending: true,
            has_finalizer: true,
            existing_finalizers: vec![FINALIZER.to_string()],
            observed_generation: 1,
            phase: Some(WorkloadPhase::Running),
            status: None,
            desired_replicas: 1,
        };
        assert_eq!(decide(&state), ReconcileDecision::HandleDeletion);
    }

    #[test]
    fn decide_missing_finalizer() {
        let workload = base_workload();
        let state = WorkloadState::from_workload(&workload);
        assert_eq!(
            decide(&state),
            ReconcileDecision::AddFinalizer {
                existing_finalizers: vec![],
            }
        );
    }

    #[test]
    fn decide_up_to_date() {
        let mut workload = base_workload();
        with_finalizer(&mut workload);
        workload.status = Some(running_status(1));
        let state = WorkloadState::from_workload(&workload);
        assert_eq!(decide(&state), ReconcileDecision::UpToDate);
    }

    #[test]
    fn decide_needs_eval_new_generation() {
        let mut workload = base_workload();
        with_finalizer(&mut workload);
        workload.metadata.generation = Some(2);
        workload.status = Some(running_status(1));
        let state = WorkloadState::from_workload(&workload);
        assert_eq!(decide(&state), ReconcileDecision::RunEval);
    }

    #[test]
    fn decide_uses_cached_eval() {
        let mut workload = base_workload();
        with_finalizer(&mut workload);
        let mut status = running_status(1);
        status.phase = WorkloadPhase::Provisioning;
        workload.status = Some(status);
        let state = WorkloadState::from_workload(&workload);
        match decide(&state) {
            ReconcileDecision::Provision { eval_result } => {
                assert_eq!(eval_result.store_path, "/nix/store/abc123-myapp-1.0");
            }
            other => panic!("expected Provision, got {:?}", other),
        }
    }

    #[test]
    fn conditions_upsert_replaces_by_type() {
        let existing = vec![
            make_condition("Evaluated", ConditionStatus::True, "Ok", "ok", 1),
            make_condition(
                "Progressing",
                ConditionStatus::True,
                "Updating",
                "updating",
                1,
            ),
        ];
        let new = vec![make_condition(
            "Evaluated",
            ConditionStatus::False,
            "EvalFailed",
            "timeout",
            2,
        )];
        let result = upsert_conditions(Some(&existing), new);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].type_, "Evaluated");
        assert_eq!(result[0].status, ConditionStatus::False);
        assert_eq!(result[0].reason, "EvalFailed");
        assert_eq!(result[1].type_, "Progressing");
        assert_eq!(result[1].status, ConditionStatus::True);
    }

    #[test]
    fn conditions_upsert_preserves_others() {
        let existing = vec![make_condition(
            "Custom",
            ConditionStatus::True,
            "Something",
            "custom condition",
            1,
        )];
        let new = vec![make_condition(
            "Evaluated",
            ConditionStatus::True,
            "EvalSucceeded",
            "ok",
            1,
        )];
        let result = upsert_conditions(Some(&existing), new);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].type_, "Custom");
        assert_eq!(result[1].type_, "Evaluated");
    }

    #[test]
    fn build_success_conditions_running_has_available() {
        let eval = EvalResponse {
            store_path: "/nix/store/abc-app".into(),
            name: "app".into(),
            main_program: Some("app".into()),
            closure_paths: vec![],
        };
        let conditions = build_success_conditions(&eval, WorkloadPhase::Running, 3, 3, 5);
        assert_eq!(conditions.len(), 3);
        assert_eq!(conditions[0].type_, "Evaluated");
        assert_eq!(conditions[0].status, ConditionStatus::True);
        assert_eq!(conditions[1].type_, "Progressing");
        assert_eq!(conditions[1].status, ConditionStatus::False);
        assert_eq!(conditions[1].reason, "NewReplicaSetAvailable");
        assert_eq!(conditions[2].type_, "Available");
        assert_eq!(conditions[2].status, ConditionStatus::True);
    }

    #[test]
    fn build_success_conditions_provisioning_no_available() {
        let eval = EvalResponse {
            store_path: "/nix/store/abc-app".into(),
            name: "app".into(),
            main_program: Some("app".into()),
            closure_paths: vec![],
        };
        let conditions = build_success_conditions(&eval, WorkloadPhase::Provisioning, 1, 3, 5);
        assert_eq!(conditions.len(), 2);
        assert_eq!(conditions[0].type_, "Evaluated");
        assert_eq!(conditions[1].type_, "Progressing");
        assert_eq!(conditions[1].status, ConditionStatus::True);
        assert_eq!(conditions[1].reason, "ReplicaSetUpdated");
        assert!(conditions.iter().all(|c| c.type_ != "Available"));
    }

    #[test]
    fn build_failed_conditions_sets_evaluated_false() {
        let conditions = build_failed_conditions("EvalTimeout", "timed out after 60s", 3);
        assert_eq!(conditions.len(), 1);
        assert_eq!(conditions[0].type_, "Evaluated");
        assert_eq!(conditions[0].status, ConditionStatus::False);
        assert_eq!(conditions[0].reason, "EvalTimeout");
        assert_eq!(conditions[0].observed_generation, Some(3));
    }
}
