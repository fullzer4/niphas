# Niphas -- Deploy Flow

## User experience

A user deploys a Nix flake on K8s with a single resource:

```yaml
apiVersion: niphas.io/v1alpha1
kind: NiphasWorkload
metadata:
  name: myapp
spec:
  flakeRef: "github:myorg/myapp"
  attribute: "packages.x86_64-linux.default"
```

No Dockerfile, no registry, no image tags. The operator handles eval,
build, caching, mounting, service creation, and failure recovery.

## Minimal deploy (3 lines)

```yaml
apiVersion: niphas.io/v1alpha1
kind: NiphasWorkload
metadata:
  name: hello
spec:
  flakeRef: "github:NixOS/nixpkgs"
  attribute: "legacyPackages.x86_64-linux.hello"
```

This evaluates the flake, resolves the store path, creates a single-replica
Deployment, mounts the closure via CSI, and runs the `hello` binary.

## Production deploy

```yaml
apiVersion: niphas.io/v1alpha1
kind: NiphasWorkload
metadata:
  name: myapp
  namespace: production
spec:
  flakeRef: "github:myorg/myapp"
  attribute: "packages.x86_64-linux.default"
  revision: "a1b2c3d4e5f6"
  replicas: 3

  args: ["--port", "8080"]

  env:
    - name: DATABASE_URL
      valueFrom:
        secretKeyRef:
          name: myapp-db
          key: url
    - name: LOG_LEVEL
      value: "info"

  ports:
    - name: http
      containerPort: 8080
    - name: metrics
      containerPort: 9090

  resources:
    requests:
      cpu: "250m"
      memory: "256Mi"
    limits:
      cpu: "1"
      memory: "1Gi"

  livenessProbe:
    httpGet:
      path: /healthz
      port: http
    initialDelaySeconds: 10
  readinessProbe:
    httpGet:
      path: /ready
      port: http

  service:
    type: ClusterIP
    ports:
      - name: http
        port: 80
        targetPort: http

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

  binaryCache: "https://cache.company.com"

  extraVolumes:
    - name: config
      configMap:
        name: myapp-config
  extraVolumeMounts:
    - name: config
      mountPath: /etc/myapp
      readOnly: true
```

## CRD fields

The NiphasWorkload replaces every Dockerfile concept:

| Dockerfile | NiphasWorkload |
|-----------|----------------|
| `FROM` / image | `spec.flakeRef` + `spec.attribute` |
| `ENTRYPOINT` | `spec.command` (auto-detected from `meta.mainProgram` if omitted) |
| `CMD` | `spec.args` |
| `EXPOSE` | `spec.ports` |
| `ENV` | `spec.env` |
| `HEALTHCHECK` | `spec.livenessProbe` / `spec.readinessProbe` |

Plus K8s-native fields: `resources`, `nodeSelector`, `tolerations`,
`affinity`, `service`, `ingress`, `extraVolumes`, `extraVolumeMounts`.

## Build model: CI builds, cluster consumes

Niphas does NOT build in-cluster. The cluster only evaluates and fetches.

```
CI (GitHub Actions, Hydra, etc.):
  nix build .#packages.x86_64-linux.default
  nix copy --to https://cache.company.com ./result
```

`nix eval` resolves the store path deterministically from the derivation
inputs (the `.drv` hash) without building. The path is pure math -- same
inputs always produce the same `/nix/store/<hash>-<name>`. The cluster
runs `nix eval` to discover the path, then fetches the pre-built NAR
from the binary cache.

If the store path is not found in any configured binary cache, the
workload fails with a clear error:

```
status.phase = "Failed"
status.conditions:
  - type: Evaluated
    status: "False"
    reason: StorePathNotCached
    message: "/nix/store/abc123-myapp-1.0.0 not found in any binary cache. Build in CI first."
```

This is intentional. In-cluster builds would execute arbitrary code with
network access, require Nix daemon privileges, and add massive complexity.
The CI is the build system. The cluster is the runtime.

## Evaluation: in-process via Nix C API (no Jobs)

niphas-eval does NOT spawn Jobs or shell out to `nix eval`. It calls the
Nix evaluator directly via C FFI using `nix-bindings-rust`, the same
approach devenv 2.0 uses in production.

The Nix C API (`libexpr-c`, `libstore-c`, `libflake-c`) exposes evaluation
as library calls. niphas-eval links against these at build time via
Rust FFI bindings. This eliminates:

- Job creation overhead (K8s API round-trip)
- Pod scheduling latency
- Container image pull for eval sandbox
- Process spawn + exit overhead
- Log scraping

Evaluation becomes a function call inside the niphas-eval process:

```rust
// Simplified -- actual implementation uses nix-bindings-rust
fn evaluate_workload(flake_ref: &str, attribute: &str) -> Result<EvalResult> {
    let ctx = Context::new()?;
    let store = Store::open(&ctx, None)?;
    let state = EvalStateBuilder::new(&store)?.build()?;

    // Construct flake installable: "github:myorg/myapp#packages.x86_64-linux.default"
    let expr = format!(
        "let drv = (builtins.getFlake \"{}\").{}; in {{ \
           outPath = drv.outPath; \
           name = drv.name; \
           mainProgram = drv.meta.mainProgram or null; \
         }}",
        flake_ref, attribute
    );

    let value = state.eval_from_string(&expr, "<niphas-eval>")?;
    let attrs = value.require_attrs()?;

    Ok(EvalResult {
        store_path: attrs.get("outPath")?.require_string()?,
        name: attrs.get("name")?.require_string()?,
        main_program: attrs.get("mainProgram")?.as_string().ok(),
    })
}
```

### Eval performance

| Scenario | Time | Why |
|----------|------|-----|
| First eval of a new flake | 5-15s | Fetches flake source + inputs from git |
| Same flake, new revision | 1-3s | Inputs cached, re-evaluates expression |
| Same flake, same revision | milliseconds | Eval cache hit |
| Redeploy (no spec change) | skipped | `observedGeneration == generation` |

The Nix eval cache persists in the niphas-eval pod's local store
(`/nix/store` on a PVC or hostPath). Cold start (first eval ever)
fetches nixpkgs and inputs -- this is expected and normal.

### Sandbox flags

Even via FFI, the evaluator runs with restrictive settings:

```rust
let mut settings = EvalSettings::default();
settings.sandbox = true;
settings.restrict_eval = true;
settings.allowed_uris = vec![
    "https://github.com",
    "https://cache.nixos.org",
];
settings.allow_import_from_derivation = false;  // critical: blocks IFD
settings.max_jobs = 0;                          // no builds, eval only
```

`allow-import-from-derivation = false` is the most important flag. IFD
would allow arbitrary builds during eval, which is the main attack vector
for malicious flakes.

### Flake allowlist (deny-by-default)

Before evaluation, niphas-eval validates the flakeRef:

```rust
fn validate_flake_ref(flake_ref: &str, allowlist: &[String]) -> Result<()> {
    for pattern in allowlist {
        if matches_glob(flake_ref, pattern) {
            return Ok(());
        }
    }
    Err(EvalError::FlakeNotAllowed(flake_ref.into()))
}
```

Config:
```yaml
allowedFlakeOrigins:
  - "github:myorg/*"
  - "github:nixos/nixpkgs"
```

Any flakeRef not matching is rejected before the evaluator is invoked.

## End-to-end sequence

```
User creates NiphasWorkload CR
    |
    v
[1] operator watches CRD, sees new resource
    sets status.phase = "Evaluating"
    |
    v
[2] operator calls niphas-eval webhook
    POST /evaluate { flakeRef, attribute, revision }
    |
    v
[3] niphas-eval validates flakeRef against allowlist
    if rejected: returns error, operator sets phase = "Failed"
    |
    v
[4] niphas-eval calls Nix evaluator via C FFI (in-process):
    - constructs flakeRef with revision: "github:myorg/myapp/<rev>"
    - evaluates: outPath, name, meta.mainProgram
    - sandbox=true, IFD=false, restrict-eval=true
    - returns EvalResult in milliseconds (warm) to seconds (cold)
    |
    v
[5] niphas-eval resolves the full closure via HTTP:
    - fetches <cache-url>/<store-path-hash>.narinfo
    - parses References field (immediate dependencies)
    - recursively fetches .narinfo for each reference
      (parallel, concurrency 16-32)
    - collects all store paths in the closure
    |
    v
[6] if root .narinfo returns 404 from all caches:
    returns error, operator sets phase = "Failed",
    reason = StorePathNotCached
    (user must build in CI and push to cache first)
    |
    v
[7] niphas-eval returns result to operator.
    operator updates CRD status:
    status.phase = "Provisioning"
    status.storePath = "/nix/store/abc123-myapp-1.0.0"
    status.resolvedCommand = "/nix/store/abc123-myapp-1.0.0/bin/myapp"
    status.closurePaths = ["/nix/store/abc123-myapp-1.0.0",
                           "/nix/store/def456-glibc-2.38", ...]
    status.conditions += { type: Evaluated, status: True }
    |
    v
[8] operator generates child resources:

    Deployment:
      - image: ghcr.io/fullzer4/niphas-runner:latest (scratch stub)
      - command: ["/nix/store/abc123-myapp-1.0.0/bin/myapp"]
      - volumes:
          - csi:
              driver: niphas.io.csi
              volumeAttributes:
                closurePaths: "/nix/store/abc123-myapp-1.0.0,..."
                mountMode: "closure"
      - volumeMounts: /nix/store (read-only)
      - env, ports, probes, resources from CRD spec
      - topology spread + failover tolerations
    Service (if spec.service set)
    Ingress (if spec.ingress set)
    PDB (if replicas >= 2)

    all child resources have ownerReferences -> NiphasWorkload
    |
    v
[9] K8s schedules pods. kubelet on each node:
    - calls NodePublishVolume on niphas-csi
    - CSI fetches closure via fallback chain:
        local cache -> mesh P2P -> binary cache HTTP
    - verifies NAR signatures (Ed25519)
    - extracts and bind-mounts at /nix/store (read-only)
    |
    v
[10] container starts, probes pass, pod becomes Ready
     |
     v
[11] operator updates status:
     phase = "Running"
     readyReplicas = 3
     conditions += { type: Available, status: True }
     |
     v
[12] niphas-mesh (if enabled) announces new NARs via gossipsub
     other nodes discover availability for future fetches
     no proactive replication -- each node fetches on demand
```

## The runner image

K8s requires an `image` field on every container. Since Niphas mounts
the actual binary from the CSI volume, the container image is just a
stub that provides a minimal Linux userspace.

`niphas-runner` is a scratch-like image (~1MB) that provides:
- `/lib64/ld-linux-x86-64.so.2` (dynamic linker, from Nix)
- Basic `/etc` files (passwd, group, nsswitch.conf)
- Nothing else. No shell, no package manager.

For statically linked Nix binaries (musl), even this is unnecessary
and a true `FROM scratch` (0 bytes) image works.

## Command resolution

When `spec.command` is omitted:

1. Eval resolves `meta.mainProgram` from the derivation
2. If set: uses `${storePath}/bin/${mainProgram}`
3. If not: lists `$out/bin/`, picks the single binary
4. If ambiguous (multiple binaries): eval fails with clear error
5. Resolved command stored in status for observability

## Updates and rollouts

When the user changes `flakeRef`, `attribute`, or `revision`:

1. `metadata.generation` increments
2. Operator detects `observedGeneration < generation`
3. Re-triggers eval (steps 2-6)
4. If store path changed: updates Deployment CSI volumeAttributes + command
5. K8s performs standard rolling update
6. Old pods drain, new pods mount new closure
7. Canary/blue-green strategies work as normal

## Multi-architecture

For mixed clusters (amd64 + arm64), use the `{arch}` template:

```yaml
spec:
  flakeRef: "github:myorg/myapp"
  attribute: "packages.{arch}-linux.default"
  architectures:
    - x86_64
    - aarch64
  replicas: 6
```

The operator:
1. Evaluates per architecture (two in-process eval calls via Nix C API)
2. Creates two Deployments (3 replicas each) with `nodeSelector`
3. Both share the label `niphas.io/workload: myapp`
4. Single Service selects both via the shared label
5. Load balances across architectures transparently

Status:
```yaml
status:
  architectures:
    - arch: x86_64
      storePath: "/nix/store/abc123-myapp-1.0.0"
      readyReplicas: 3
    - arch: aarch64
      storePath: "/nix/store/xyz789-myapp-1.0.0"
      readyReplicas: 3
  readyReplicas: 6
```

## Helm chart for user workloads

Users in Helm-based workflows can deploy via a convenience chart:

```bash
helm install myapp niphas/workload \
  --set flakeRef=github:myorg/myapp \
  --set attribute=packages.x86_64-linux.default \
  --set replicas=3 \
  --set ports[0].name=http \
  --set ports[0].containerPort=8080 \
  --set service.enabled=true \
  --set service.ports[0].port=80 \
  --set service.ports[0].targetPort=http \
  --set ingress.enabled=true \
  --set ingress.hosts[0].host=myapp.example.com
```

Or with a values file:

```yaml
# myapp-values.yaml
flakeRef: "github:myorg/myapp"
attribute: "packages.x86_64-linux.default"
revision: "a1b2c3d"
replicas: 3
ports:
  - name: http
    containerPort: 8080
resources:
  requests:
    cpu: "250m"
    memory: "256Mi"
service:
  enabled: true
  ports:
    - name: http
      port: 80
      targetPort: http
ingress:
  enabled: true
  className: nginx
  hosts:
    - host: myapp.example.com
      paths:
        - path: /
          port: http
```

```bash
helm install myapp niphas/workload -f myapp-values.yaml
```

The chart generates a NiphasWorkload CR from the values.

## Helm chart for Niphas platform

Installing Niphas itself:

```bash
# 1. Install the platform
helm install niphas niphas/niphas -n niphas-system --create-namespace

# 2. Label nodes that should run Niphas workloads
kubectl label node worker-{1..5} niphas.io/store=true

# 3. Optional: enable mesh for P2P NAR sharing
kubectl label node worker-{1..5} niphas.io/mesh=true
```

With custom binary cache and mesh disabled:

```bash
helm install niphas niphas/niphas -n niphas-system --create-namespace \
  --set csi.binaryCaches[0].url=https://cache.company.com \
  --set csi.binaryCaches[0].publicKey="company-1:abc..." \
  --set mesh.enabled=false
```

The platform chart deploys: operator (Deployment), CSI driver (DaemonSet
with `nodeSelector: niphas.io/store=true`), mesh (DaemonSet with
`nodeSelector: niphas.io/mesh=true`, optional via `mesh.enabled`),
eval webhook (Deployment), CRDs, RBAC, NetworkPolicies, PriorityClasses.

Nodes must be labeled before workloads can be scheduled on them:

```bash
kubectl label node worker-1 worker-2 worker-3 niphas.io/store=true
# Optional: enable mesh for P2P NAR sharing between nodes
kubectl label node worker-1 worker-2 worker-3 niphas.io/mesh=true
```

## GitOps integration

NiphasWorkload CRDs work with ArgoCD and Flux out of the box.

ArgoCD health check (add to argocd-cm ConfigMap):

```lua
-- resource.customizations.health.niphas.io_NiphasWorkload
hs = {}
if obj.status ~= nil then
  if obj.status.phase == "Running" then
    hs.status = "Healthy"
  elseif obj.status.phase == "Failed" then
    hs.status = "Degraded"
  else
    hs.status = "Progressing"
  end
  hs.message = "Phase: " .. (obj.status.phase or "Unknown")
end
return hs
```

Flux uses `observedGeneration` from the status to detect drift
(already implemented in the CRD).

## Service exposure

### Embedded (simple case)

The CRD includes `spec.service` and `spec.ingress`. The operator
creates both with `ownerReferences`. Covers 80% of use cases.

### Standalone (advanced case)

For Gateway API, multiple services, or service mesh routing, the user
creates standard K8s resources that select pods via label:

```yaml
selector:
  niphas.io/workload: myapp
```

The operator labels all generated pods with `niphas.io/workload: <name>`,
making them selectable by any K8s networking resource.
