# Niphas -- Operator Design

niphas-operator reconciles `NiphasWorkload` CRDs into running Kubernetes
workloads. It is the central control plane component.

## Reconciliation state machine

```
                    ┌──────────────┐
       CRD created  │              │
       ────────────>│   Pending    │
                    │              │
                    └──────┬───────┘
                           │
                  set phase = Evaluating
                  call eval webhook
                           │
                    ┌──────▼───────┐
                    │              │  eval error
                    │  Evaluating  │──────────────┐
                    │              │               │
                    └──────┬───────┘               │
                           │                      │
                  eval returns EvalResult         │
                  set storePath, closurePaths      │
                  set phase = Provisioning         │
                           │                      │
                    ┌──────▼───────┐               │
                    │              │  create error │
                    │ Provisioning │──────────┐    │
                    │              │          │    │
                    └──────┬───────┘          │    │
                           │                 │    │
                  child resources created    │    │
                  pods becoming ready        │    │
                           │                 │    │
                    ┌──────▼───────┐         │    │
                    │              │         │    │
                    │   Running    │         │    │
                    │              │         │    │
                    └──────┬───────┘         │    │
                           │          ┌─────▼────▼──┐
                  spec changed        │             │
                  (generation++)      │   Failed    │
                  ───────────────>    │             │
                  back to Evaluating  └─────────────┘
```

### Transition rules

| From | To | Trigger |
|------|----|---------|
| (none) | Pending | CRD created |
| Pending | Evaluating | Operator sees new resource |
| Evaluating | Provisioning | Eval webhook returns success |
| Evaluating | Failed | Eval error (timeout, flake not allowed, eval crash) |
| Provisioning | Running | At least 1 replica is Ready |
| Provisioning | Failed | Child resource creation fails |
| Running | Evaluating | `observedGeneration < generation` (spec updated) |
| Running | Degraded | Some replicas not ready, NAR corruption detected |
| Failed | Evaluating | User updates spec (generation increments) |

## Reconciler loop

The operator uses kube-rs `Controller` with a single reconciler function.

```rust
async fn reconcile(
    workload: Arc<NiphasWorkload>,
    ctx: Arc<Context>,
) -> Result<Action, Error> {
    let name = workload.name_any();
    let ns = workload.namespace().unwrap();
    let generation = workload.metadata.generation.unwrap_or(0);

    // 1. Check if we already processed this generation
    let status = workload.status.as_ref();
    let observed = status.and_then(|s| s.observed_generation).unwrap_or(0);

    if observed >= generation && status.map(|s| s.phase.as_str()) == Some("Running") {
        // Nothing to do. Requeue in 5 minutes for health check.
        return Ok(Action::requeue(Duration::from_secs(300)));
    }

    // 2. Evaluate (if needed)
    let eval_result = if needs_eval(&workload, observed, generation) {
        set_phase(&ctx, &workload, "Evaluating").await?;
        match call_eval_webhook(&ctx, &workload).await {
            Ok(result) => result,
            Err(e) => {
                set_failed(&ctx, &workload, "EvalFailed", &e.to_string()).await?;
                return Ok(Action::requeue(Duration::from_secs(30)));
            }
        }
    } else {
        // Use cached eval result from status
        EvalResult::from_status(status.unwrap())
    };

    // 3. Provision child resources
    set_phase(&ctx, &workload, "Provisioning").await?;
    apply_child_resources(&ctx, &workload, &eval_result).await?;

    // 4. Check readiness
    let ready = count_ready_replicas(&ctx, &workload).await?;
    let desired = workload.spec.replicas.unwrap_or(1);

    let phase = if ready >= desired { "Running" } else { "Provisioning" };
    update_status(&ctx, &workload, phase, &eval_result, ready, generation).await?;

    // 5. Requeue
    let interval = if ready < desired {
        Duration::from_secs(5)   // poll frequently while provisioning
    } else {
        Duration::from_secs(300) // stable, check every 5 min
    };
    Ok(Action::requeue(interval))
}
```

### needs_eval()

```rust
fn needs_eval(workload: &NiphasWorkload, observed: i64, generation: i64) -> bool {
    // New or updated resource
    if observed < generation { return true; }

    // No store path yet (first reconciliation after crash)
    let status = workload.status.as_ref();
    status.and_then(|s| s.store_path.as_ref()).is_none()
}
```

## Eval webhook integration

The operator calls niphas-eval via HTTP (internal Service, not public):

### Request

```
POST http://niphas-eval.niphas-system.svc:8443/evaluate
Content-Type: application/json
```

```json
{
  "flakeRef": "github:myorg/myapp",
  "attribute": "packages.x86_64-linux.default",
  "revision": "a1b2c3d4e5f6",
  "binaryCache": "https://cache.company.com"
}
```

`revision` and `binaryCache` are optional. If `binaryCache` is omitted,
niphas-eval uses the caches from its own ConfigMap.

### Response (success)

```json
{
  "storePath": "/nix/store/abc123-myapp-1.0.0",
  "name": "myapp-1.0.0",
  "mainProgram": "myapp",
  "closurePaths": [
    "/nix/store/abc123-myapp-1.0.0",
    "/nix/store/def456-glibc-2.38",
    "/nix/store/ghi789-openssl-3.1"
  ]
}
```

### Response (error)

```json
{
  "error": "StorePathNotCached",
  "message": "/nix/store/abc123-myapp-1.0.0 not found in any binary cache"
}
```

Error codes:

| Code | Meaning | Operator action |
|------|---------|-----------------|
| `FlakeNotAllowed` | flakeRef not in allowlist | Set Failed, reason=FlakeNotAllowed |
| `EvalFailed` | Nix evaluation error | Set Failed, reason=EvalFailed |
| `EvalTimeout` | Evaluation exceeded timeout | Set Failed, reason=EvalTimeout |
| `StorePathNotCached` | NAR not in any binary cache | Set Failed, reason=StorePathNotCached |
| `ClosureResolutionFailed` | Could not resolve full closure | Set Failed, reason=ClosureResolutionFailed |

### Timeout

The operator sets a per-request timeout of 300 seconds (configurable).
If the eval webhook does not respond within this window, the operator
cancels the request and sets `Failed` with reason `EvalTimeout`.

### Multi-architecture

When `spec.architectures` is set, the operator calls the eval webhook
once per architecture, substituting `{arch}` in the attribute:

```
POST /evaluate { attribute: "packages.x86_64-linux.default", ... }
POST /evaluate { attribute: "packages.aarch64-linux.default", ... }
```

These calls are made concurrently (`tokio::join!`).

## Child resource generation

After successful eval, the operator creates/updates these child resources:

### Deployment

Generated for every NiphasWorkload. If `spec.architectures` is set,
one Deployment per architecture.

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: <workload>                    # or <workload>-<arch> for multi-arch
  namespace: <namespace>
  ownerReferences: [...]
  labels:
    niphas.io/workload: <workload>
    niphas.io/managed-by: niphas-operator
spec:
  replicas: <spec.replicas or 1>
  selector:
    matchLabels:
      niphas.io/workload: <workload>
  template:
    metadata:
      labels:
        niphas.io/workload: <workload>
        niphas.io/store-hash: <short-hash>
    spec:
      nodeSelector:
        niphas.io/store: "true"
        # + spec.nodeSelector (merged)
        # + kubernetes.io/arch: <arch> (if multi-arch)

      tolerations:
        # fast failover (always injected)
        - key: node.kubernetes.io/not-ready
          operator: Exists
          effect: NoExecute
          tolerationSeconds: 30
        - key: node.kubernetes.io/unreachable
          operator: Exists
          effect: NoExecute
          tolerationSeconds: 30
        # + spec.tolerations (appended)

      topologySpreadConstraints:       # only if replicas >= 2
        - maxSkew: 1
          topologyKey: kubernetes.io/hostname
          whenUnsatisfiable: DoNotSchedule
          labelSelector:
            matchLabels:
              niphas.io/workload: <workload>
        - maxSkew: 1
          topologyKey: topology.kubernetes.io/zone
          whenUnsatisfiable: ScheduleAnyway
          labelSelector:
            matchLabels:
              niphas.io/workload: <workload>

      containers:
        - name: app
          image: ghcr.io/fullzer4/niphas-runner:latest
          command: [<resolvedCommand>]
          args: <spec.args>
          env: <spec.env>
          ports: <spec.ports>
          resources: <spec.resources>
          livenessProbe: <spec.livenessProbe>
          readinessProbe: <spec.readinessProbe>
          startupProbe: <spec.startupProbe>
          volumeMounts:
            - name: nix-store
              mountPath: /nix/store
              readOnly: true
            # + spec.extraVolumeMounts

      volumes:
        - name: nix-store
          csi:
            driver: niphas.io.csi
            volumeAttributes:
              closurePaths: "<comma-separated closure paths>"
        # + spec.extraVolumes
```

### Service (conditional)

Created only if `spec.service` is set.

```yaml
apiVersion: v1
kind: Service
metadata:
  name: <workload>
  namespace: <namespace>
  ownerReferences: [...]
spec:
  type: <spec.service.type or ClusterIP>
  selector:
    niphas.io/workload: <workload>
  ports: <spec.service.ports>
```

### Ingress (conditional)

Created only if `spec.ingress` is set and `spec.ingress.enabled` is true.

```yaml
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: <workload>
  namespace: <namespace>
  ownerReferences: [...]
spec:
  ingressClassName: <spec.ingress.className>
  rules: <spec.ingress.hosts>
  tls: <spec.ingress.tls>
```

### PodDisruptionBudget (conditional)

Created only if `spec.replicas >= 2`.

```yaml
apiVersion: policy/v1
kind: PodDisruptionBudget
metadata:
  name: <workload>-pdb
  namespace: <namespace>
  ownerReferences: [...]
spec:
  maxUnavailable: 1
  selector:
    matchLabels:
      niphas.io/workload: <workload>
  unhealthyPodEvictionPolicy: AlwaysAllow
```

## Server-side apply

The operator uses **server-side apply** (SSA) for all child resources:

```rust
let patch = Patch::Apply(&deployment);
let params = PatchParams::apply("niphas-operator").force();
deployments.patch(&name, &params, &patch).await?;
```

Benefits:
- Idempotent: applying the same spec twice is a no-op
- Conflict detection: if another controller modified the resource, SSA
  merges fields by field manager
- No read-modify-write race: atomic update

## Update and rollout

When the user changes `flakeRef`, `attribute`, `revision`, or any spec field:

1. `metadata.generation` increments (K8s does this automatically)
2. Reconciler detects `observedGeneration < generation`
3. If `flakeRef`, `attribute`, or `revision` changed: re-evaluates
4. If store path changed: updates Deployment's CSI volumeAttributes + command
5. K8s performs standard rolling update (default strategy: `RollingUpdate`)
6. Old pods drain, new pods mount new closure via CSI
7. Operator watches replica readiness, updates status

For spec-only changes (e.g. `replicas`, `resources`, `env`) that don't
affect the Nix closure, the operator skips eval and directly patches
the Deployment.

```rust
fn needs_eval(workload: &NiphasWorkload, observed: i64, generation: i64) -> bool {
    if observed < generation {
        // Check if only non-closure fields changed
        let prev = workload.status.as_ref();
        let same_flake = prev.map(|s| /* compare flakeRef, attribute, revision */);
        return !same_flake.unwrap_or(false);
    }
    false
}
```

## Finalizer

The operator adds `niphas.io/workload-cleanup` to every NiphasWorkload.

### Deletion flow

```
1. User deletes NiphasWorkload
2. K8s sets deletionTimestamp (object is now Terminating)
3. Reconciler runs:
   a. Sees deletionTimestamp is set
   b. Child resources have ownerReferences --> K8s GC deletes them
   c. Operator removes the finalizer
4. K8s completes deletion
```

The finalizer exists to ensure the reconciler runs at least once during
deletion (e.g. to emit events, update metrics). The actual resource
cleanup is handled by K8s garbage collection via ownerReferences.

```rust
async fn reconcile(workload: Arc<NiphasWorkload>, ctx: Arc<Context>) -> Result<Action, Error> {
    // Handle deletion first
    if workload.metadata.deletion_timestamp.is_some() {
        // Emit deletion event
        ctx.recorder.publish(Event {
            type_: EventType::Normal,
            reason: "Deleting".into(),
            note: Some(format!("Cleaning up workload {}", workload.name_any())),
            action: "Delete".into(),
            secondary: None,
        }).await?;

        // Remove finalizer (child resources cleaned up by ownerReferences)
        finalizer::remove(&ctx.client, &workload).await?;
        return Ok(Action::await_change());
    }

    // Ensure finalizer exists
    if !finalizer::exists(&workload) {
        finalizer::add(&ctx.client, &workload).await?;
        return Ok(Action::requeue(Duration::from_secs(0)));
    }

    // ... normal reconciliation
}
```

## Leader election

The operator runs with 2-3 replicas for HA. Only the leader reconciles.

```rust
let lease = LeaseLock::new(client.clone(), "niphas-operator", LeaseLockParams {
    holder_id: pod_name,
    lease_ttl: Duration::from_secs(15),
    retry_period: Duration::from_secs(5),
    renew_period: Duration::from_secs(5),
});
```

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| TTL | 15s | Time before a dead leader is considered gone |
| Renew | 5s | Leader heartbeat interval |
| Retry | 5s | Non-leader check interval |

Failover takes at most ~15 seconds. Reconciliation is idempotent,
so brief dual-leader windows (unlikely but possible) are safe.

## Error handling and retry

| Error type | Retry strategy |
|------------|---------------|
| Eval webhook timeout | Requeue after 30s, retry up to 3 times, then set Failed |
| Eval webhook unreachable | Requeue after 10s, exponential backoff up to 5 min |
| Child resource conflict (SSA) | Immediate retry (SSA handles conflicts) |
| K8s API transient error | Requeue after 5s, exponential backoff |
| K8s API permanent error (403, 404) | Set Failed, do not retry |

The kube-rs `Controller` provides built-in exponential backoff for
reconciler errors. The operator supplements this with domain-specific
retry logic for eval webhook calls.

## Watches

The controller watches these resources for changes:

| Resource | Why |
|----------|-----|
| `NiphasWorkload` | Primary resource (triggers reconciliation) |
| `Deployment` (owned) | Detect readiness changes, rollout progress |
| `Pod` (owned) | Count ready replicas, detect failures |

Watches use `ownerReferences` filtering: only Deployments/Pods owned
by a NiphasWorkload trigger reconciliation of that specific workload.

```rust
Controller::new(workloads, Default::default())
    .owns(deployments, Default::default())
    .owns(pods, Default::default())
    .run(reconcile, error_policy, ctx)
```

## Health probes

The operator exposes an Axum HTTP server on port 8080:

| Endpoint | Purpose |
|----------|---------|
| `/healthz` | Liveness probe. Returns 200 if the process is alive |
| `/readyz` | Readiness probe. Returns 200 if the leader election is resolved and the controller is watching |
| `/metrics` | Prometheus metrics (reconciliation count, duration, errors) |

## Events

The operator emits Kubernetes Events for key lifecycle transitions:

| Event | Type | Reason |
|-------|------|--------|
| Eval started | Normal | `EvalStarted` |
| Eval succeeded | Normal | `EvalSucceeded` |
| Eval failed | Warning | `EvalFailed` |
| Flake not allowed | Warning | `FlakeNotAllowed` |
| Store path not cached | Warning | `StorePathNotCached` |
| Child resources created | Normal | `Provisioned` |
| All replicas ready | Normal | `Available` |
| Replica degraded | Warning | `Degraded` |
| Workload deleting | Normal | `Deleting` |

Events are visible via `kubectl describe niphasworkload <name>`.
