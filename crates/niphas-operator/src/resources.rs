use crate::context::Context;
use crate::error::OperatorError;
use crate::eval::EvalResult;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Service;
use k8s_openapi::api::networking::v1::Ingress;
use k8s_openapi::api::policy::v1::PodDisruptionBudget;
use kube::api::{Api, Patch, PatchParams, ResourceExt};
use niphas_core::crd::NiphasWorkload;
use serde_json::json;
use tracing::debug;

const FIELD_MANAGER: &str = "niphas-operator";
const RUNNER_IMAGE: &str = "ghcr.io/fullzer4/niphas-runner:latest";

/// Apply all child resources for a NiphasWorkload using server-side apply.
pub async fn apply_child_resources(
    ctx: &Context,
    workload: &NiphasWorkload,
    eval_result: &EvalResult,
) -> Result<(), OperatorError> {
    let name = workload.name_any();
    let ns = workload.namespace().unwrap_or_default();

    let deployment = build_deployment(workload, eval_result, &name, &ns);
    apply_resource::<Deployment>(ctx, &name, &ns, &deployment).await?;

    if workload.spec.service.is_some() {
        let service = build_service(workload, &name, &ns);
        apply_resource::<Service>(ctx, &name, &ns, &service).await?;
    }

    if let Some(ref ingress_spec) = workload.spec.ingress {
        if ingress_spec.enabled {
            let ingress = build_ingress(workload, &name, &ns);
            apply_resource::<Ingress>(ctx, &name, &ns, &ingress).await?;
        }
    }

    let replicas = workload.spec.replicas.unwrap_or(1);
    if replicas >= 2 {
        let pdb_name = format!("{name}-pdb");
        let pdb = build_pdb(workload, &name, &ns);
        apply_resource::<PodDisruptionBudget>(ctx, &pdb_name, &ns, &pdb).await?;
    }

    Ok(())
}

/// Generic SSA apply for any K8s resource type.
async fn apply_resource<K>(
    ctx: &Context,
    name: &str,
    ns: &str,
    manifest: &serde_json::Value,
) -> Result<(), OperatorError>
where
    K: kube::api::Resource<Scope = k8s_openapi::NamespaceResourceScope>
        + serde::de::DeserializeOwned
        + Clone
        + std::fmt::Debug,
    <K as kube::api::Resource>::DynamicType: Default,
{
    let api: Api<K> = Api::namespaced(ctx.client.clone(), ns);
    debug!(%name, kind = std::any::type_name::<K>(), "applying via SSA");
    api.patch(
        name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(manifest),
    )
    .await?;
    Ok(())
}

pub(crate) fn build_deployment(
    workload: &NiphasWorkload,
    eval_result: &EvalResult,
    name: &str,
    ns: &str,
) -> serde_json::Value {
    let replicas = workload.spec.replicas.unwrap_or(1);
    let resolved_command = eval_result.resolved_command();
    let closure_csv = eval_result.closure_paths.join(",");

    let owner_ref = owner_reference(workload);

    let mut node_selector = serde_json::Map::new();
    node_selector.insert("niphas.io/store".into(), json!("true"));
    if let Some(ref user_ns) = workload.spec.node_selector {
        for (k, v) in user_ns {
            node_selector.insert(k.clone(), json!(v));
        }
    }

    let mut tolerations = vec![
        json!({
            "key": "node.kubernetes.io/not-ready",
            "operator": "Exists",
            "effect": "NoExecute",
            "tolerationSeconds": 30
        }),
        json!({
            "key": "node.kubernetes.io/unreachable",
            "operator": "Exists",
            "effect": "NoExecute",
            "tolerationSeconds": 30
        }),
    ];
    if let Some(ref user_tols) = workload.spec.tolerations {
        for t in user_tols {
            tolerations.push(serde_json::to_value(t).unwrap_or_default());
        }
    }

    let command = if let Some(ref cmd) = workload.spec.command {
        json!(cmd)
    } else {
        json!([resolved_command])
    };

    let mut container = json!({
        "name": "app",
        "image": RUNNER_IMAGE,
        "command": command,
        "volumeMounts": [{
            "name": "nix-store",
            "mountPath": "/nix/store",
            "readOnly": true
        }]
    });

    let container_obj = container.as_object_mut().unwrap();

    if let Some(ref args) = workload.spec.args {
        container_obj.insert("args".into(), json!(args));
    }
    if let Some(ref env) = workload.spec.env {
        container_obj.insert("env".into(), serde_json::to_value(env).unwrap_or_default());
    }
    if let Some(ref ports) = workload.spec.ports {
        container_obj.insert(
            "ports".into(),
            serde_json::to_value(ports).unwrap_or_default(),
        );
    }
    if let Some(ref resources) = workload.spec.resources {
        container_obj.insert(
            "resources".into(),
            serde_json::to_value(resources).unwrap_or_default(),
        );
    }
    if let Some(ref probe) = workload.spec.liveness_probe {
        container_obj.insert(
            "livenessProbe".into(),
            serde_json::to_value(probe).unwrap_or_default(),
        );
    }
    if let Some(ref probe) = workload.spec.readiness_probe {
        container_obj.insert(
            "readinessProbe".into(),
            serde_json::to_value(probe).unwrap_or_default(),
        );
    }
    if let Some(ref probe) = workload.spec.startup_probe {
        container_obj.insert(
            "startupProbe".into(),
            serde_json::to_value(probe).unwrap_or_default(),
        );
    }

    if let Some(ref extra_mounts) = workload.spec.extra_volume_mounts {
        if let Some(mounts) = container_obj.get_mut("volumeMounts") {
            if let Some(arr) = mounts.as_array_mut() {
                for m in extra_mounts {
                    arr.push(serde_json::to_value(m).unwrap_or_default());
                }
            }
        }
    }

    let mut volumes = vec![json!({
        "name": "nix-store",
        "csi": {
            "driver": "niphas.io.csi",
            "volumeAttributes": {
                "storePath": eval_result.store_path,
                "closurePaths": closure_csv
            }
        }
    })];
    if let Some(ref extra_vols) = workload.spec.extra_volumes {
        for v in extra_vols {
            volumes.push(serde_json::to_value(v).unwrap_or_default());
        }
    }

    let topology_spread = if replicas >= 2 {
        json!([
            {
                "maxSkew": 1,
                "topologyKey": "kubernetes.io/hostname",
                "whenUnsatisfiable": "DoNotSchedule",
                "labelSelector": {
                    "matchLabels": {
                        "niphas.io/workload": name
                    }
                }
            },
            {
                "maxSkew": 1,
                "topologyKey": "topology.kubernetes.io/zone",
                "whenUnsatisfiable": "ScheduleAnyway",
                "labelSelector": {
                    "matchLabels": {
                        "niphas.io/workload": name
                    }
                }
            }
        ])
    } else {
        json!(null)
    };

    let mut pod_spec = json!({
        "nodeSelector": node_selector,
        "tolerations": tolerations,
        "containers": [container],
        "volumes": volumes,
    });

    if topology_spread != json!(null) {
        pod_spec
            .as_object_mut()
            .unwrap()
            .insert("topologySpreadConstraints".into(), topology_spread);
    }

    if let Some(ref affinity) = workload.spec.affinity {
        pod_spec.as_object_mut().unwrap().insert(
            "affinity".into(),
            serde_json::to_value(affinity).unwrap_or_default(),
        );
    }

    json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": {
            "name": name,
            "namespace": ns,
            "ownerReferences": [owner_ref],
            "labels": {
                "niphas.io/workload": name,
                "niphas.io/managed-by": "niphas-operator"
            }
        },
        "spec": {
            "replicas": replicas,
            "selector": {
                "matchLabels": {
                    "niphas.io/workload": name
                }
            },
            "template": {
                "metadata": {
                    "labels": {
                        "niphas.io/workload": name,
                        "niphas.io/managed-by": "niphas-operator"
                    }
                },
                "spec": pod_spec,
            }
        }
    })
}

pub(crate) fn build_service(workload: &NiphasWorkload, name: &str, ns: &str) -> serde_json::Value {
    let svc_spec = workload.spec.service.as_ref().unwrap();
    let owner_ref = owner_reference(workload);

    let ports: Vec<serde_json::Value> = svc_spec
        .ports
        .iter()
        .map(|p| {
            let mut port = json!({
                "port": p.port,
                "protocol": p.protocol.as_deref().unwrap_or("TCP"),
            });
            let obj = port.as_object_mut().unwrap();
            if let Some(ref name) = p.name {
                obj.insert("name".into(), json!(name));
            }
            if let Some(ref tp) = p.target_port {
                if let Ok(num) = tp.parse::<i32>() {
                    obj.insert("targetPort".into(), json!(num));
                } else {
                    obj.insert("targetPort".into(), json!(tp));
                }
            }
            port
        })
        .collect();

    let svc_type = svc_spec.type_.as_deref().unwrap_or("ClusterIP");

    json!({
        "apiVersion": "v1",
        "kind": "Service",
        "metadata": {
            "name": name,
            "namespace": ns,
            "ownerReferences": [owner_ref],
            "labels": {
                "niphas.io/workload": name,
                "niphas.io/managed-by": "niphas-operator"
            }
        },
        "spec": {
            "type": svc_type,
            "selector": {
                "niphas.io/workload": name
            },
            "ports": ports
        }
    })
}

pub(crate) fn build_ingress(workload: &NiphasWorkload, name: &str, ns: &str) -> serde_json::Value {
    let ingress_spec = workload.spec.ingress.as_ref().unwrap();
    let owner_ref = owner_reference(workload);

    let mut spec = serde_json::Map::new();

    if let Some(ref class_name) = ingress_spec.class_name {
        spec.insert("ingressClassName".into(), json!(class_name));
    }

    if let Some(ref hosts) = ingress_spec.hosts {
        let rules: Vec<serde_json::Value> = hosts
            .iter()
            .map(|h| {
                let paths: Vec<serde_json::Value> = h
                    .paths
                    .iter()
                    .map(|p| {
                        let port_val = if let Ok(num) = p.port.parse::<i32>() {
                            json!({"number": num})
                        } else {
                            json!({"name": p.port})
                        };
                        json!({
                            "path": p.path,
                            "pathType": p.path_type.as_deref().unwrap_or("Prefix"),
                            "backend": {
                                "service": {
                                    "name": name,
                                    "port": port_val
                                }
                            }
                        })
                    })
                    .collect();
                json!({
                    "host": h.host,
                    "http": {
                        "paths": paths
                    }
                })
            })
            .collect();
        spec.insert("rules".into(), json!(rules));
    }

    if let Some(ref tls) = ingress_spec.tls {
        let tls_entries: Vec<serde_json::Value> = tls
            .iter()
            .map(|t| {
                json!({
                    "hosts": t.hosts,
                    "secretName": t.secret_name
                })
            })
            .collect();
        spec.insert("tls".into(), json!(tls_entries));
    }

    json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "Ingress",
        "metadata": {
            "name": name,
            "namespace": ns,
            "ownerReferences": [owner_ref],
            "labels": {
                "niphas.io/workload": name,
                "niphas.io/managed-by": "niphas-operator"
            }
        },
        "spec": spec
    })
}

pub(crate) fn build_pdb(workload: &NiphasWorkload, name: &str, ns: &str) -> serde_json::Value {
    let pdb_name = format!("{name}-pdb");
    let owner_ref = owner_reference(workload);

    json!({
        "apiVersion": "policy/v1",
        "kind": "PodDisruptionBudget",
        "metadata": {
            "name": pdb_name,
            "namespace": ns,
            "ownerReferences": [owner_ref],
            "labels": {
                "niphas.io/workload": name,
                "niphas.io/managed-by": "niphas-operator"
            }
        },
        "spec": {
            "maxUnavailable": 1,
            "selector": {
                "matchLabels": {
                    "niphas.io/workload": name
                }
            },
            "unhealthyPodEvictionPolicy": "AlwaysAllow"
        }
    })
}

fn owner_reference(workload: &NiphasWorkload) -> serde_json::Value {
    json!({
        "apiVersion": "niphas.io/v1alpha1",
        "kind": "NiphasWorkload",
        "name": workload.name_any(),
        "uid": workload.metadata.uid.as_deref().unwrap_or(""),
        "controller": true,
        "blockOwnerDeletion": true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use niphas_core::crd::{
        NiphasIngressHost, NiphasIngressPath, NiphasIngressSpec, NiphasIngressTls,
        NiphasServicePort, NiphasServiceSpec, NiphasWorkloadSpec,
    };

    fn test_workload(name: &str) -> NiphasWorkload {
        NiphasWorkload {
            metadata: kube::api::ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some("default".to_string()),
                uid: Some("test-uid-1234".to_string()),
                ..Default::default()
            },
            spec: NiphasWorkloadSpec {
                flake_ref: "github:test/app".into(),
                attribute: "packages.x86_64-linux.default".into(),
                revision: None,
                replicas: Some(1),
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

    fn test_eval_result() -> EvalResult {
        EvalResult {
            store_path: "/nix/store/abc123-myapp-1.0.0".into(),
            name: "myapp-1.0.0".into(),
            main_program: Some("myapp".into()),
            closure_paths: vec![
                "/nix/store/abc123-myapp-1.0.0".into(),
                "/nix/store/def456-glibc-2.37".into(),
            ],
        }
    }

    #[test]
    fn test_build_deployment_basic() {
        let workload = test_workload("myapp");
        let eval = test_eval_result();
        let dep = build_deployment(&workload, &eval, "myapp", "default");

        assert_eq!(dep["apiVersion"], "apps/v1");
        assert_eq!(dep["kind"], "Deployment");
        assert_eq!(dep["spec"]["replicas"], 1);
        assert_eq!(dep["metadata"]["name"], "myapp");
        assert_eq!(dep["metadata"]["namespace"], "default");

        // Check CSI volume
        let volumes = dep["spec"]["template"]["spec"]["volumes"]
            .as_array()
            .unwrap();
        assert_eq!(volumes[0]["csi"]["driver"], "niphas.io.csi");
        assert_eq!(
            volumes[0]["csi"]["volumeAttributes"]["storePath"],
            "/nix/store/abc123-myapp-1.0.0",
            "storePath must be passed in volumeAttributes"
        );
        assert_eq!(
            volumes[0]["csi"]["volumeAttributes"]["closurePaths"],
            "/nix/store/abc123-myapp-1.0.0,/nix/store/def456-glibc-2.37"
        );

        // Check nodeSelector includes niphas.io/store=true
        let ns = &dep["spec"]["template"]["spec"]["nodeSelector"];
        assert_eq!(ns["niphas.io/store"], "true");

        // Check command resolves to main_program
        let containers = dep["spec"]["template"]["spec"]["containers"]
            .as_array()
            .unwrap();
        let cmd = containers[0]["command"].as_array().unwrap();
        assert_eq!(cmd[0], "/nix/store/abc123-myapp-1.0.0/bin/myapp");
    }

    #[test]
    fn test_build_deployment_topology_spread_multi_replica() {
        let mut workload = test_workload("myapp");
        workload.spec.replicas = Some(3);
        let eval = test_eval_result();
        let dep = build_deployment(&workload, &eval, "myapp", "default");

        assert_eq!(dep["spec"]["replicas"], 3);
        let tsc = dep["spec"]["template"]["spec"]["topologySpreadConstraints"]
            .as_array()
            .expect("should have topologySpreadConstraints for replicas >= 2");
        assert_eq!(tsc.len(), 2);
        assert_eq!(tsc[0]["topologyKey"], "kubernetes.io/hostname");
        assert_eq!(tsc[1]["topologyKey"], "topology.kubernetes.io/zone");
    }

    #[test]
    fn test_build_deployment_no_topology_single() {
        let workload = test_workload("myapp");
        let eval = test_eval_result();
        let dep = build_deployment(&workload, &eval, "myapp", "default");

        assert!(
            dep["spec"]["template"]["spec"]["topologySpreadConstraints"].is_null(),
            "single replica should not have topologySpreadConstraints"
        );
    }

    #[test]
    fn test_build_deployment_custom_command() {
        let mut workload = test_workload("myapp");
        workload.spec.command = Some(vec!["/custom/bin".into(), "--flag".into()]);
        let eval = test_eval_result();
        let dep = build_deployment(&workload, &eval, "myapp", "default");

        let containers = dep["spec"]["template"]["spec"]["containers"]
            .as_array()
            .unwrap();
        let cmd = containers[0]["command"].as_array().unwrap();
        assert_eq!(cmd[0], "/custom/bin");
        assert_eq!(cmd[1], "--flag");
    }

    #[test]
    fn test_build_deployment_owner_ref() {
        let workload = test_workload("myapp");
        let eval = test_eval_result();
        let dep = build_deployment(&workload, &eval, "myapp", "default");

        let refs = dep["metadata"]["ownerReferences"].as_array().unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0]["apiVersion"], "niphas.io/v1alpha1");
        assert_eq!(refs[0]["kind"], "NiphasWorkload");
        assert_eq!(refs[0]["controller"], true);
        assert_eq!(refs[0]["uid"], "test-uid-1234");
    }

    #[test]
    fn test_build_service_cluster_ip() {
        let mut workload = test_workload("myapp");
        workload.spec.service = Some(NiphasServiceSpec {
            type_: None,
            ports: vec![NiphasServicePort {
                name: Some("http".into()),
                port: 80,
                target_port: Some("8080".into()),
                protocol: None,
            }],
        });

        let svc = build_service(&workload, "myapp", "default");
        assert_eq!(svc["spec"]["type"], "ClusterIP");
        assert_eq!(svc["spec"]["selector"]["niphas.io/workload"], "myapp");

        let ports = svc["spec"]["ports"].as_array().unwrap();
        assert_eq!(ports[0]["port"], 80);
        assert_eq!(ports[0]["targetPort"], 8080);
        assert_eq!(ports[0]["name"], "http");
    }

    #[test]
    fn test_build_ingress_with_tls() {
        let mut workload = test_workload("myapp");
        workload.spec.ingress = Some(NiphasIngressSpec {
            enabled: true,
            class_name: Some("nginx".into()),
            hosts: Some(vec![NiphasIngressHost {
                host: "app.example.com".into(),
                paths: vec![NiphasIngressPath {
                    path: "/".into(),
                    path_type: None,
                    port: "80".into(),
                }],
            }]),
            tls: Some(vec![NiphasIngressTls {
                secret_name: "app-tls".into(),
                hosts: vec!["app.example.com".into()],
            }]),
        });

        let ing = build_ingress(&workload, "myapp", "default");
        assert_eq!(ing["spec"]["ingressClassName"], "nginx");

        let tls = ing["spec"]["tls"].as_array().unwrap();
        assert_eq!(tls.len(), 1);
        assert_eq!(tls[0]["secretName"], "app-tls");

        let rules = ing["spec"]["rules"].as_array().unwrap();
        assert_eq!(rules[0]["host"], "app.example.com");
    }

    #[test]
    fn test_build_pdb() {
        let workload = test_workload("myapp");
        let pdb = build_pdb(&workload, "myapp", "default");

        assert_eq!(pdb["metadata"]["name"], "myapp-pdb");
        assert_eq!(pdb["spec"]["maxUnavailable"], 1);
        assert_eq!(
            pdb["spec"]["selector"]["matchLabels"]["niphas.io/workload"],
            "myapp"
        );
    }

    #[test]
    fn test_owner_reference_shape() {
        let workload = test_workload("myapp");
        let oref = owner_reference(&workload);

        assert_eq!(oref["apiVersion"], "niphas.io/v1alpha1");
        assert_eq!(oref["kind"], "NiphasWorkload");
        assert_eq!(oref["name"], "myapp");
        assert_eq!(oref["controller"], true);
        assert_eq!(oref["blockOwnerDeletion"], true);
    }

    // -- EvalResult tests --

    #[test]
    fn test_resolved_command_with_main_program() {
        let eval = EvalResult {
            store_path: "/nix/store/abc-hello-2.12.1".into(),
            name: "hello-2.12.1".into(),
            main_program: Some("hello".into()),
            closure_paths: vec![],
        };
        assert_eq!(
            eval.resolved_command(),
            "/nix/store/abc-hello-2.12.1/bin/hello"
        );
    }

    #[test]
    fn test_resolved_command_without_main_program() {
        let eval = EvalResult {
            store_path: "/nix/store/abc-hello-2.12.1".into(),
            name: "hello-2.12.1".into(),
            main_program: None,
            closure_paths: vec![],
        };
        assert_eq!(
            eval.resolved_command(),
            "/nix/store/abc-hello-2.12.1/bin/hello-2.12.1"
        );
    }
}
