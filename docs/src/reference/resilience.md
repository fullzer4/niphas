# Resilience & Failure Handling

How niphas handles node failures, cache misses, component crashes, and maintains availability.

## Node Failure Timeline

```
T+0s     Node stops heartbeating
T+40s    Taint: NotReady + unreachable:NoExecute
T+70s    Pods evicted (Niphas tolerationSeconds=30s, not default 300s)
T+70s    Pods rescheduled to healthy nodes
```

Niphas overrides the default 300s toleration to 30s for faster failover.

## Fetch Fallback Chain

When a pod is rescheduled, the CSI driver fetches NARs through a 4-source fallback:

```
1. Local cache     ─ instant, always valid
       │ miss
2. Mesh P2P        ─ LAN transfer, 5s timeout (optional)
       │ miss
3. Binary cache    ─ WAN HTTPS, priority-ordered
       │ miss
4. gRPC Unavailable ─ kubelet retries with backoff
```

## Per-Node Lazy Cache

Each node only caches NARs for pods it actually runs — no proactive replication.

**Benefits:**
- Minimal resource usage
- No bandwidth overhead
- Scales to any cluster size
- No coordination needed

## PodDisruptionBudget

Created automatically for workloads with `replicas >= 2`:

- `maxUnavailable: 1`
- `unhealthyPodEvictionPolicy: AlwaysAllow` (K8s 1.31+)

## Topology Spread

- **Hard constraint:** no two replicas on the same node
- **Soft constraint:** prefer spreading across zones (don't block scheduling)

## PriorityClass

| Component | Priority | Class |
|-----------|----------|-------|
| CSI DaemonSet | system-cluster-critical | Must survive resource pressure |
| Operator, Mesh | 1,000,000 | `niphas-infrastructure` |
| Workloads | 100,000 | `niphas-workload` |

## Leader Election

Operator uses Lease-based leader election:

- 15s TTL, 5s renewal
- New leader acquired within ~15s on failure
- Reconciliation is idempotent — brief dual-leader windows are safe

## CSI Error Handling

| gRPC Code | Meaning | Action |
|-----------|---------|--------|
| `Unavailable` | Transient error | kubelet retries |
| `Internal` | Mount failure | kubelet retries |
| `InvalidArgument` | User configuration error | Requires fix |
| `NotFound` | Store path doesn't exist | Requires fix |

Never leaves a partial mount — operations are atomic.

## Status Conditions

Five condition types track the workload state:

- **Evaluated** — eval succeeded or failed
- **ClosureCached** — all paths available in cache
- **Available** — minimum replicas ready
- **Progressing** — rollout in progress
- **Degraded** — partial failure

All tracked via `observedGeneration` for eventual consistency.

## Failure Scenario Matrix

| Failure | Impact | Recovery |
|---------|--------|----------|
| Node dies | Pods rescheduled in ~70s | CSI re-fetches NARs |
| Binary cache down | New pods can't start | Mesh + local cache still work |
| Mesh + cache down | New pods can't start | Wait for binary cache |
| CSI pod crashes | DaemonSet restarts it | Cached data persists |
| Operator dies | No reconciliation | Leader re-elected in ~15s |
| Eval pod dies | Evals fail | Deployment restarts, PVC preserves cache |
| NAR corrupted | Re-hash detects | Evict and re-fetch |
| CRD deleted | Finalizer cleanup | ownerReferences cascade delete |
| All nodes die | Total outage | Restore from binary cache |
