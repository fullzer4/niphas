# Operator Design

The niphas-operator watches `NiphasWorkload` custom resources and reconciles them into running Kubernetes workloads.

## Reconciliation State Machine

```
                    ┌──────────┐
          create    │ Pending  │
         ────────►  └────┬─────┘
                         │
                    ┌────▼─────┐
          eval ok   │Evaluating│──── eval error ──► Failed
                    └────┬─────┘
                         │
                   ┌─────▼──────┐
         apply ok  │Provisioning│──── apply error ─► Failed
                   └─────┬──────┘
                         │
                    ┌────▼─────┐
         ready      │ Running  │──── spec change ──► Evaluating
                    └──────────┘
```

## Reconciler Loop

Each reconciliation:

1. Compare `observedGeneration` vs `metadata.generation`
2. If eval needed → call eval webhook (POST to niphas-eval)
3. On success → generate child resources (Deployment, Service, Ingress, PDB)
4. Apply via server-side apply (SSA) for idempotence
5. Check replica readiness → update phase
6. Requeue with interval

## Eval Webhook Integration

The operator calls `POST /evaluate` on niphas-eval:

**Request:**
```json
{
  "flakeRef": "github:myorg/myapp",
  "attribute": "packages.x86_64-linux.default",
  "revision": "abc123",
  "binaryCache": "https://cache.example.com"
}
```

**Response (200):**
```json
{
  "storePath": "/nix/store/abc...-myapp-1.0",
  "name": "myapp-1.0",
  "mainProgram": "myapp",
  "closurePaths": ["/nix/store/abc...-myapp-1.0", "/nix/store/def...-glibc-2.38", "..."]
}
```

**Error codes:** `FlakeNotAllowed` (403), `EvalFailed` (422), `EvalTimeout` (408), `StorePathNotCached` (404), `ClosureResolutionFailed` (502)

## Child Resources

The operator generates and applies:

- **Deployment** — with `nodeSelector` (`niphas.io/store=true`), CSI ephemeral volume, runner image, tolerations for fast failover
- **Service** — conditional, created only if `spec.service` is set
- **Ingress** — conditional, created only if `spec.ingress.enabled` is true
- **PodDisruptionBudget** — created when `replicas >= 2`, `maxUnavailable=1`

All child resources use `ownerReferences` pointing back to the `NiphasWorkload`.

## Leader Election

Lease-based leader election with:
- 15s TTL
- 5s renewal interval
- 5s retry interval

New leader acquired within ~15s on failure. Reconciliation is idempotent, so brief dual-leader windows are safe.

## Error Handling

| Error | Strategy |
|-------|----------|
| Eval timeout | Requeue after 30s |
| Eval unreachable | Exponential backoff |
| SSA conflict | Immediate retry |
| Transient K8s error | 5s + backoff |
| Permanent error | Set phase `Failed` |

## Events

The operator emits Kubernetes Events:

`EvalStarted`, `EvalSucceeded`, `EvalFailed`, `FlakeNotAllowed`, `StorePathNotCached`, `Provisioned`, `Available`, `Degraded`, `Deleting`

## Health Probes

- `/healthz` — process alive
- `/readyz` — leader acquired and watching
- `/metrics` — Prometheus endpoint
