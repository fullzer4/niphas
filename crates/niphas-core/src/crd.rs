use k8s_openapi::api::core::v1::{
    Affinity, ContainerPort, EnvVar, Probe, ResourceRequirements, Toleration, Volume, VolumeMount,
};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "niphas.io",
    version = "v1alpha1",
    kind = "NiphasWorkload",
    namespaced,
    status = "NiphasWorkloadStatus",
    shortname = "nw",
    shortname = "niphas",
    printcolumn = r#"{"name":"Flake","type":"string","jsonPath":".spec.flakeRef"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Ready","type":"string","jsonPath":".status.readyReplicas"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct NiphasWorkloadSpec {
    /// Flake reference. E.g. `github:myorg/myapp`.
    pub flake_ref: String,

    /// Flake output attribute path. E.g. `packages.x86_64-linux.default`.
    pub attribute: String,

    /// Git revision to pin. Must be a 6-40 char hex string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,

    /// Number of pod replicas. Defaults to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replicas: Option<i32>,

    /// Override binary cache URL for this workload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_cache: Option<String>,

    /// Override entrypoint. If omitted, resolved from `meta.mainProgram`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,

    /// Arguments passed to the command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,

    /// Environment variables (same schema as K8s container.env).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<Vec<EnvVar>>,

    /// Exposed ports (same schema as K8s container.ports).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ports: Option<Vec<ContainerPort>>,

    /// CPU/memory requests and limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourceRequirements>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub liveness_probe: Option<Probe>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness_probe: Option<Probe>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_probe: Option<Probe>,

    /// Additional node selector labels (merged with `niphas.io/store=true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_selector: Option<std::collections::BTreeMap<String, String>>,

    /// Extra tolerations. Operator always injects not-ready/unreachable (30s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tolerations: Option<Vec<Toleration>>,

    /// Pod affinity/anti-affinity rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affinity: Option<Affinity>,

    /// If set, the operator creates a Service with ownerReferences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<NiphasServiceSpec>,

    /// If set, the operator creates an Ingress with ownerReferences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingress: Option<NiphasIngressSpec>,

    /// Additional K8s volumes (ConfigMaps, Secrets, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_volumes: Option<Vec<Volume>>,

    /// Mount points for extra volumes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_volume_mounts: Option<Vec<VolumeMount>>,

    /// If set, evaluate per-architecture. Uses `{arch}` template in `attribute`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architectures: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NiphasServiceSpec {
    /// Service type: ClusterIP, NodePort, LoadBalancer. Defaults to ClusterIP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,

    /// Service ports.
    pub ports: Vec<NiphasServicePort>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NiphasServicePort {
    /// Port name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Service port number.
    pub port: i32,

    /// Target port (name or number as string).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_port: Option<String>,

    /// Protocol (TCP, UDP). Defaults to TCP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NiphasIngressSpec {
    /// Whether to create the Ingress resource.
    #[serde(default)]
    pub enabled: bool,

    /// Ingress class name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_name: Option<String>,

    /// Ingress host rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosts: Option<Vec<NiphasIngressHost>>,

    /// TLS configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<Vec<NiphasIngressTls>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NiphasIngressHost {
    /// Hostname.
    pub host: String,

    /// Paths for this host.
    pub paths: Vec<NiphasIngressPath>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NiphasIngressPath {
    /// URL path.
    pub path: String,

    /// Path type: Prefix, Exact, ImplementationSpecific.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_type: Option<String>,

    /// Backend port (name or number as string).
    pub port: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NiphasIngressTls {
    /// Secret name containing TLS cert.
    pub secret_name: String,

    /// Hostnames covered by this TLS config.
    pub hosts: Vec<String>,
}

/// High-level phase of a NiphasWorkload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub enum WorkloadPhase {
    #[default]
    Pending,
    Evaluating,
    Provisioning,
    Running,
    Failed,
}

impl WorkloadPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Evaluating => "Evaluating",
            Self::Provisioning => "Provisioning",
            Self::Running => "Running",
            Self::Failed => "Failed",
        }
    }
}

impl std::fmt::Display for WorkloadPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Condition status following the K8s convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ConditionStatus {
    True,
    False,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct NiphasWorkloadStatus {
    /// The `metadata.generation` most recently processed by the controller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,

    /// High-level phase: Pending, Evaluating, Provisioning, Running, Failed.
    #[serde(default)]
    pub phase: WorkloadPhase,

    /// Resolved Nix store path. E.g. `/nix/store/abc123-myapp-1.0.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_path: Option<String>,

    /// The actual entrypoint command. E.g. `/nix/store/abc123-myapp-1.0.0/bin/myapp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_command: Option<String>,

    /// Full transitive closure (root + all references).
    /// The CSI driver uses this to fetch NARs without depending on binary cache .narinfo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closure_paths: Option<Vec<String>>,

    /// ISO 8601 timestamp of last successful evaluation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_eval: Option<String>,

    /// Number of pods in Ready state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_replicas: Option<i32>,

    /// Standard K8s-style conditions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<NiphasCondition>>,

    /// Per-architecture status (only when `spec.architectures` is set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architectures: Option<Vec<ArchStatus>>,
}

/// Per-architecture status for multi-arch workloads.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArchStatus {
    /// Architecture name (e.g. `x86_64`, `aarch64`).
    pub arch: String,

    /// Resolved store path for this architecture.
    pub store_path: String,

    /// Resolved entrypoint for this architecture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_command: Option<String>,

    /// Closure paths for this architecture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closure_paths: Option<Vec<String>>,

    /// Ready replicas for this architecture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_replicas: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NiphasCondition {
    /// Condition type: Evaluated, ClosureCached, Available, Progressing, Degraded.
    #[serde(rename = "type")]
    pub type_: String,

    /// Condition status.
    pub status: ConditionStatus,

    /// Machine-readable reason: EvalSucceeded, NarFetchFailed, ReplicasReady, etc.
    pub reason: String,

    /// Human-readable message.
    pub message: String,

    /// When this condition last transitioned (ISO 8601).
    pub last_transition_time: String,

    /// The generation this condition was set at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}
