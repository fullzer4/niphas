# Niphas -- CRD Reference

Single source of truth for the `NiphasWorkload` custom resource.

```yaml
apiVersion: niphas.io/v1alpha1
kind: NiphasWorkload
```

## Spec

### Core fields (required)

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `flakeRef` | `string` | yes | Flake reference. E.g. `github:myorg/myapp` |
| `attribute` | `string` | yes | Flake output attribute. E.g. `packages.x86_64-linux.default` |

### Core fields (optional)

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `revision` | `string` | latest | Git revision to pin. E.g. `a1b2c3d4e5f6` |
| `replicas` | `i32` | `1` | Number of pod replicas |
| `binaryCache` | `string` | from ConfigMap | Override binary cache URL for this workload |

### Container fields

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `command` | `Vec<string>` | auto-detected | Override entrypoint. If omitted, resolved from `meta.mainProgram` during eval |
| `args` | `Vec<string>` | none | Arguments passed to the command |
| `env` | `Vec<EnvVar>` | none | Environment variables. Same schema as K8s `container.env` |
| `ports` | `Vec<ContainerPort>` | none | Exposed ports. Same schema as K8s `container.ports` |

### Resource management

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `resources` | `ResourceRequirements` | none | CPU/memory requests and limits. Same schema as K8s |

### Health checks

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `livenessProbe` | `Probe` | none | Same schema as K8s `container.livenessProbe` |
| `readinessProbe` | `Probe` | none | Same schema as K8s `container.readinessProbe` |
| `startupProbe` | `Probe` | none | Same schema as K8s `container.startupProbe` |

### Scheduling

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `nodeSelector` | `Map<string,string>` | none | Additional node selector labels (merged with `niphas.io/store=true`) |
| `tolerations` | `Vec<Toleration>` | failover tolerations | Extra tolerations. The operator always injects `not-ready`/`unreachable` tolerations (30s) |
| `affinity` | `Affinity` | none | Pod affinity/anti-affinity rules |

### Service exposure

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `service` | `ServiceSpec` | none | If set, the operator creates a Service with `ownerReferences` |
| `ingress` | `IngressSpec` | none | If set, the operator creates an Ingress with `ownerReferences` |

#### ServiceSpec

```yaml
service:
  type: ClusterIP           # ClusterIP, NodePort, LoadBalancer
  ports:
    - name: http
      port: 80
      targetPort: http       # name or number
```

#### IngressSpec

```yaml
ingress:
  enabled: true
  className: nginx
  hosts:
    - host: myapp.example.com
      paths:
        - path: /
          pathType: Prefix
          port: http
  tls:
    - secretName: myapp-tls
      hosts:
        - myapp.example.com
```

### Extra volumes

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `extraVolumes` | `Vec<Volume>` | none | Additional K8s volumes (ConfigMaps, Secrets, etc.) |
| `extraVolumeMounts` | `Vec<VolumeMount>` | none | Mount points for extra volumes |

### Multi-architecture

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `architectures` | `Vec<string>` | none | If set, evaluate per-architecture. Uses `{arch}` template in `attribute`. E.g. `["x86_64", "aarch64"]` |

When `architectures` is set:
- The `attribute` field must contain `{arch}` placeholder
- Operator evaluates once per architecture
- Creates separate Deployments per architecture with `nodeSelector`
- All Deployments share the label `niphas.io/workload: <name>`
- A single Service selects all architectures via the shared label

---

## Status

All status fields are set by the operator. Users do not write to status.

### Top-level status

| Field | Type | Description |
|-------|------|-------------|
| `observedGeneration` | `i64` | The `metadata.generation` most recently processed. If `< generation`, the operator hasn't processed the latest spec change |
| `phase` | `string` | High-level state. One of: `Pending`, `Evaluating`, `Provisioning`, `Running`, `Failed` |
| `storePath` | `string` | Resolved Nix store path. E.g. `/nix/store/abc123-myapp-1.0.0` |
| `resolvedCommand` | `string` | The actual entrypoint. E.g. `/nix/store/abc123-myapp-1.0.0/bin/myapp` |
| `closurePaths` | `Vec<string>` | Full closure (root + all transitive references). CSI uses this to fetch without depending on binary cache `.narinfo` |
| `lastEval` | `string` | ISO 8601 timestamp of last successful evaluation |
| `readyReplicas` | `i32` | Number of pods in Ready state |
| `conditions` | `Vec<Condition>` | Standard K8s-style conditions |

### Multi-architecture status

When `spec.architectures` is set, status includes per-arch details:

| Field | Type | Description |
|-------|------|-------------|
| `architectures` | `Vec<ArchStatus>` | Per-architecture status |

```yaml
status:
  architectures:
    - arch: x86_64
      storePath: "/nix/store/abc123-myapp-1.0.0"
      resolvedCommand: "/nix/store/abc123-myapp-1.0.0/bin/myapp"
      readyReplicas: 3
    - arch: aarch64
      storePath: "/nix/store/xyz789-myapp-1.0.0"
      resolvedCommand: "/nix/store/xyz789-myapp-1.0.0/bin/myapp"
      readyReplicas: 3
  readyReplicas: 6   # sum of all archs
```

### Phase lifecycle

```
Pending --> Evaluating --> Provisioning --> Running
   |            |               |
   v            v               v
 Failed       Failed          Failed
```

| Phase | Meaning |
|-------|---------|
| `Pending` | CRD created, operator has not started reconciliation |
| `Evaluating` | Operator called eval webhook, waiting for result |
| `Provisioning` | Eval succeeded, operator is creating child resources (Deployment, Service, etc.) |
| `Running` | At least one replica is Ready |
| `Failed` | Eval failed, NAR not found in cache, or child resource creation failed |

Transitions back to `Evaluating` happen when `observedGeneration < generation`
(spec was updated).

### Condition types

| Type | True | False |
|------|------|-------|
| `Evaluated` | Nix eval succeeded, store path resolved | Eval failed, pending, or flake not allowed |
| `ClosureCached` | All closure paths available in binary cache | Fetch failed or pending |
| `Available` | >= 1 replica running and Ready | No replicas ready |
| `Progressing` | Rollout in progress (new spec being applied) | Stable |
| `Degraded` | Partial failure (some replicas down, NAR corrupted, etc.) | Everything healthy |

#### Condition struct

```yaml
conditions:
  - type: Evaluated
    status: "True"              # "True", "False", "Unknown"
    reason: EvalSucceeded       # machine-readable
    message: "store path resolved: /nix/store/abc123-myapp-1.0.0"
    lastTransitionTime: "2026-06-05T12:00:00Z"
    observedGeneration: 3       # generation when this condition was set
```

#### Reason values

| Condition | Reason (True) | Reason (False) |
|-----------|--------------|----------------|
| `Evaluated` | `EvalSucceeded` | `EvalFailed`, `FlakeNotAllowed`, `EvalTimeout` |
| `ClosureCached` | `CacheVerified` | `StorePathNotCached`, `NarFetchFailed` |
| `Available` | `ReplicasReady` | `NoReplicasReady` |
| `Progressing` | `RolloutInProgress` | `RolloutComplete` |
| `Degraded` | `PartialFailure`, `NarCorrupted`, `NarSignatureVerificationFailed` | `Healthy` |

---

## Validation rules

| Field | Rule |
|-------|------|
| `flakeRef` | Must match `^[a-zA-Z][a-zA-Z0-9+.-]*:.+$` (valid flake reference syntax) |
| `attribute` | Must not be empty |
| `replicas` | >= 0 |
| `revision` | If set, must match `^[a-f0-9]{6,40}$` (git short or full SHA) |
| `ports[].containerPort` | 1-65535, unique within the array |
| `architectures[]` | Each must be a valid Nix system arch: `x86_64`, `aarch64`, `i686`, `armv7l` |
| `architectures` + `attribute` | If `architectures` is set, `attribute` must contain `{arch}` |

## Defaults injected by the operator

The operator merges these into the generated Deployment spec:

| What | Value | Source |
|------|-------|--------|
| `nodeSelector` | `niphas.io/store: "true"` | Always injected, merged with `spec.nodeSelector` |
| `tolerations` | `not-ready`/`unreachable` with 30s | Always injected for fast failover |
| `topologySpreadConstraints` | hostname spread (hard), zone spread (soft) | Injected for replicas >= 2 |
| `labels` | `niphas.io/workload: <name>` | On all pods, for Service selection |
| `volumes` | CSI inline volume for `/nix/store` | Always, from closure data |
| `volumeMounts` | `/nix/store` (read-only) | Always |
| `image` | `ghcr.io/fullzer4/niphas-runner:latest` | Stub image, overridable |
| `command` | `[resolvedCommand]` | From eval result |

## Labels and annotations set by the operator

### On child resources (Deployment, Service, Ingress, PDB)

```yaml
labels:
  niphas.io/workload: <workload-name>
  niphas.io/managed-by: niphas-operator
  app.kubernetes.io/name: <workload-name>
  app.kubernetes.io/managed-by: niphas
annotations:
  niphas.io/store-path: /nix/store/abc123-myapp-1.0.0
  niphas.io/flake-ref: github:myorg/myapp
  niphas.io/revision: a1b2c3d4e5f6
```

### On pods

```yaml
labels:
  niphas.io/workload: <workload-name>     # for Service selection
  niphas.io/store-hash: abc123            # short hash for identification
```

## ownerReferences

All child resources have `ownerReferences` pointing to the NiphasWorkload:

```yaml
ownerReferences:
  - apiVersion: niphas.io/v1alpha1
    kind: NiphasWorkload
    name: <workload-name>
    uid: <workload-uid>
    controller: true
    blockOwnerDeletion: true
```

K8s garbage collection automatically deletes child resources when the
NiphasWorkload is deleted (after finalizer cleanup).

## Print columns (kubectl output)

```
$ kubectl get niphasworkloads
NAME    FLAKE                    PHASE     READY
myapp   github:myorg/myapp       Running   3
hello   github:NixOS/nixpkgs     Running   1
```

Defined via `printcolumn` in the CRD derive:

| Column | JSONPath |
|--------|----------|
| Flake | `.spec.flakeRef` |
| Phase | `.status.phase` |
| Ready | `.status.readyReplicas` |

## Short names

```yaml
shortNames:
  - nw
  - niphas
```

`kubectl get nw` is equivalent to `kubectl get niphasworkloads`.
