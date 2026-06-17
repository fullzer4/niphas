# CRD Reference

Complete API reference for the `NiphasWorkload` custom resource.

## Spec — Core Fields

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `flakeRef` | string | yes | — | Nix flake reference |
| `attribute` | string | yes | — | Flake output attribute |
| `revision` | string | no | latest | Git revision (6–40 hex chars) |
| `replicas` | integer | no | 1 | Number of pod replicas |
| `binaryCache` | string | no | ConfigMap | Binary cache URL |

## Spec — Container Fields

| Field | Type | Description |
|-------|------|-------------|
| `command` | string | Entrypoint (auto-detected from `meta.mainProgram` if omitted) |
| `args` | list of strings | Arguments passed to the entrypoint |
| `env` | list of EnvVar | Environment variables (standard K8s schema) |
| `ports` | list of ContainerPort | Container port definitions |

## Spec — Resource Management

| Field | Type | Description |
|-------|------|-------------|
| `resources` | ResourceRequirements | CPU/memory requests and limits |

## Spec — Health Checks

| Field | Type | Description |
|-------|------|-------------|
| `livenessProbe` | Probe | Liveness probe (standard K8s schema) |
| `readinessProbe` | Probe | Readiness probe |
| `startupProbe` | Probe | Startup probe |

## Spec — Scheduling

| Field | Type | Description |
|-------|------|-------------|
| `nodeSelector` | map | Merged with `niphas.io/store=true` |
| `tolerations` | list | Appended to failover tolerations |
| `affinity` | Affinity | Standard K8s affinity rules |

## Spec — Service Exposure

| Field | Type | Description |
|-------|------|-------------|
| `service` | ServiceSpec | Service type and ports |
| `ingress` | IngressSpec | `enabled`, `className`, `hosts`, `tls` |

## Spec — Multi-Architecture

| Field | Type | Description |
|-------|------|-------------|
| `architectures` | list of strings | Target Nix systems |

When set, `attribute` must contain `{arch}` placeholder. One Deployment per architecture, single Service selects all.

## Status

| Field | Type | Description |
|-------|------|-------------|
| `observedGeneration` | integer | Last reconciled generation |
| `phase` | string | `Pending`, `Evaluating`, `Provisioning`, `Running`, `Failed` |
| `storePath` | string | Resolved Nix store path |
| `resolvedCommand` | string | Resolved entrypoint binary |
| `closurePaths` | list of strings | Full transitive closure |
| `lastEval` | timestamp | Last successful evaluation time |
| `readyReplicas` | integer | Number of ready replicas |
| `conditions` | list of Condition | Detailed status conditions |

## Conditions

| Type | True | False |
|------|------|-------|
| `Evaluated` | Eval succeeded | Eval failed or pending |
| `ClosureCached` | All paths available | Missing paths |
| `Available` | Minimum replicas ready | Insufficient replicas |
| `Progressing` | Rollout in progress | Stable |
| `Degraded` | Partial failure | Fully healthy |

## Validation Rules

- `flakeRef` must match allowed regex pattern
- `attribute` must not be empty
- `replicas` must be >= 0
- `revision` must be 6–40 hex characters
- Port names must be unique
- `architectures` must be valid Nix system strings
- `{arch}` required in `attribute` when `architectures` is set

## Defaults Injected by Operator

- `nodeSelector` always includes `niphas.io/store=true`
- Tolerations injected for fast failover (30s vs default 300s)
- `topologySpreadConstraints` for `replicas >= 2`
- CSI volume for `/nix/store`
- Runner image as container image
- Command resolved from eval if not specified

## Labels on Child Resources

```
niphas.io/workload: <name>
niphas.io/managed-by: niphas-operator
niphas.io/store-path: <hash>
niphas.io/flake-ref: <ref>
app.kubernetes.io/name: <name>
app.kubernetes.io/managed-by: niphas
```

## kubectl Output

```
NAME    FLAKE                    PHASE     READY
hello   github:NixOS/nixpkgs    Running   1/1
```

Short names: `nw`, `niphas`
