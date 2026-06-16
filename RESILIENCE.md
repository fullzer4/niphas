# Niphas -- Resilience & Failure Handling

Every design decision assumes the worst case: nodes die, networks partition,
caches go offline, and disks corrupt. Niphas follows K8s-native patterns
for all failure scenarios.

## Node failure timeline

When a node dies, K8s follows this sequence:

| Time | Event |
|------|-------|
| T+0 | Node stops heartbeating |
| T+40s | `node-monitor-grace-period` expires. Node tainted `NotReady` + `unreachable:NoExecute` |
| T+40s + tolerationSeconds | Pods evicted. Default tolerationSeconds = 300s (5 min) |
| T+5m40s | Pods rescheduled on healthy nodes |

Niphas workloads override the default toleration for faster failover:

```yaml
tolerations:
  - key: node.kubernetes.io/not-ready
    operator: Exists
    effect: NoExecute
    tolerationSeconds: 30
  - key: node.kubernetes.io/unreachable
    operator: Exists
    effect: NoExecute
    tolerationSeconds: 30
```

Total failover: ~70 seconds instead of ~5m40s.

## What happens when a pod is rescheduled

```
node-A dies
  --> K8s evicts pods after tolerationSeconds
  --> scheduler places pod on node-B
  --> kubelet on node-B calls NodePublishVolume on niphas-csi
  --> niphas-csi needs the closure
  --> fetch chain:
      1. local cache /var/lib/niphas/cache/ (instant)
      2. niphas-mesh peers on other nodes (LAN, fast)
      3. binary cache HTTP (WAN, slower)
      4. fail --> kubelet retries with backoff
```

The pod enters `ContainerCreating` during fetch. kubelet retries
`NodePublishVolume` with exponential backoff up to 5 minutes,
identical to `ImagePullBackOff` behavior.

## Fetch fallback chain

The CSI driver tries sources in order. Each source is independent;
failure of one does not block the next.

```
1. Local NAR cache     --> /var/lib/niphas/cache/<hash>-<name>/
                            instant, no network
                            always valid (content-addressed)
                            common case for workloads already running on this node

2. Mesh P2P (libp2p)   --> query local NAR index (gossipsub-populated)
                            "who has /nix/store/abc123?"
                            fetch NAR directly from peer (LAN speed)
                            timeout: 5s (configurable)
                            only available if mesh is enabled on this node

3. Binary caches (HTTP) --> ordered by priority:
                            a. primary: company cache (e.g. Attic/Cachix)
                            b. secondary: cache.nixos.org
                            verify NarHash + signature

4. gRPC Unavailable     --> kubelet retries with backoff
```

Config via ConfigMap:

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: niphas-csi-config
  namespace: niphas-system
data:
  config.yaml: |
    binaryCaches:
      - url: "https://cache.company.com"
        priority: 1
        publicKey: "cache.company.com-1:..."
      - url: "https://cache.nixos.org"
        priority: 2
        publicKey: "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY="
    meshFetchEnabled: true
    meshFetchTimeout: 5s
    cache:
      path: /var/lib/niphas/cache
      highWatermark: 85   # start GC at 85% disk usage
      lowWatermark: 75    # GC down to 75%
```

## Per-node lazy cache (no full replication)

Niphas does **not** replicate closures to all nodes. Each node only
caches the NARs that its pods actually used. This is the standard
approach for storage drivers (Longhorn, OpenEBS, Rook-Ceph).

When a pod is scheduled on a node, the CSI driver fetches the closure
through the fallback chain above. After extraction, the NAR stays in
the local cache at `/var/lib/niphas/cache/` for future use.

Benefits:
- **Minimal resource usage**: nodes without Niphas workloads use zero
  disk, zero RAM, zero CPU
- **No bandwidth overhead**: no background replication traffic
- **Scales to any cluster size**: 10 nodes or 10,000 nodes, same model
- **Simple**: no coordination, no anti-entropy, no cluster-wide sync

The mesh (optional) accelerates fetches by allowing nodes to pull NARs
from peers that already have them (LAN speed vs WAN binary cache).
Nodes announce cached NARs via gossipsub `Have` messages so peers know
where to find them.

Details: [`docs/MESH_PROTOCOL.md`](MESH_PROTOCOL.md)

## Selective deployment

CSI and mesh DaemonSets only run on nodes labeled `niphas.io/store=true`.
Nodes without this label have zero Niphas overhead.

```bash
# Enable Niphas on specific nodes
kubectl label node worker-1 worker-2 worker-3 niphas.io/store=true

# Mesh is optional -- enable per-node
kubectl label node worker-1 worker-2 worker-3 niphas.io/mesh=true
```

The operator adds `nodeSelector: { niphas.io/store: "true" }` to
generated Deployments so pods only land on prepared nodes.

See the Helm chart section in [`docs/DEPLOY_FLOW.md`](DEPLOY_FLOW.md)
for installation details.

## PodDisruptionBudget

The operator creates a PDB for every NiphasWorkload with replicas >= 2:

```yaml
apiVersion: policy/v1
kind: PodDisruptionBudget
metadata:
  name: <workload>-pdb
  ownerReferences:
    - apiVersion: niphas.io/v1alpha1
      kind: NiphasWorkload
      name: <workload>
      controller: true
spec:
  maxUnavailable: 1
  selector:
    matchLabels:
      niphas.io/workload: <workload>
  unhealthyPodEvictionPolicy: AlwaysAllow
```

- `maxUnavailable: 1` adapts automatically when replica count changes
- `unhealthyPodEvictionPolicy: AlwaysAllow` prevents broken pods from
  blocking node drains (stable since K8s 1.31)
- No PDB for single-replica workloads (would block all voluntary disruptions)

## Topology spread

The operator injects topology constraints into workload pod templates:

```yaml
topologySpreadConstraints:
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
```

- Node spread: hard constraint (no two replicas on same node)
- Zone spread: best-effort (don't block scheduling if zones are unbalanced)
- If replicas > nodes, allows multiple pods per node gracefully

## PriorityClass

Infrastructure pods must survive resource pressure:

```yaml
# CSI DaemonSet -- must be on every labeled node
priorityClassName: system-cluster-critical

# Operator + Mesh -- important but below system-critical
apiVersion: scheduling.k8s.io/v1
kind: PriorityClass
metadata:
  name: niphas-infrastructure
value: 1000000
preemptionPolicy: PreemptLowerPriority

# User workloads
apiVersion: scheduling.k8s.io/v1
kind: PriorityClass
metadata:
  name: niphas-workload
value: 100000
preemptionPolicy: PreemptLowerPriority
```

CSI uses `system-cluster-critical` because if the driver is evicted,
all pods on that node lose their volumes. Same pattern as EBS CSI,
GCE PD CSI, and every other production storage driver.

## Leader election (operator)

The operator runs with 2-3 replicas for HA. Only the leader reconciles.
Uses Lease-based election (coordination.k8s.io/v1):

- Lease TTL: 15 seconds
- Renewal: every 5 seconds
- On leader death: new leader acquired within ~15s
- Reconciliation is idempotent, so brief dual-leader windows are safe

```rust
// niphas-operator uses kube-leader-election crate
let lease = LeaseLock::new(client, "niphas-operator", LeaseLockParams {
    holder_id: pod_name,
    lease_ttl: Duration::from_secs(15),
    retry_period: Duration::from_secs(5),
    renew_period: Duration::from_secs(5),
});
```

## Finalizers

The operator adds `niphas.io/workload-cleanup` to every NiphasWorkload.
On deletion:
1. Operator sees `deletionTimestamp` is set
2. Cleans up: removes child resources (Deployment, Service, PDB)
3. Removes the finalizer
4. K8s completes deletion
5. CSI local cache GC eventually evicts unused NARs (LRU)

If cleanup fails, the finalizer stays, the object remains in
`Terminating`, and the controller retries on next reconciliation.

The operator manages the finalizer. No mesh-level finalizer needed --
cache cleanup is handled by per-node LRU GC.

## CSI error handling

`NodePublishVolume` follows strict idempotency:

```
1. Already mounted correctly?  --> return Ok (idempotent)
2. Partial mount from previous failure?  --> cleanup first
3. Fetch closure (fallback chain)
4. Mount
5. On any failure: clean up partial state, return gRPC error
```

gRPC error codes:
- `Unavailable` -- transient (cache down, mesh timeout). kubelet retries aggressively
- `Internal` -- mount syscall failed. kubelet retries with backoff
- `InvalidArgument` -- bad storePath in volumeAttributes. User must fix CRD
- `NotFound` -- store path doesn't exist in any cache

Critical: **never leave a partial mount at target_path on failure**.
kubelet may skip `NodePublishVolume` if it sees a filesystem at the
target path, assuming the mount succeeded.

## Status conditions (eventual consistency)

The CRD status follows K8s conventions for detecting desync:

```yaml
status:
  observedGeneration: 3          # matches metadata.generation when in sync
  phase: Running
  storePath: "/nix/store/abc123-hello-2.12.1"
  closurePaths:                  # full closure for rescheduling
    - "/nix/store/abc123-hello-2.12.1"
    - "/nix/store/def456-glibc-2.38"
  readyReplicas: 3
  conditions:
    - type: Evaluated
      status: "True"
      reason: EvalSucceeded
      message: "flake eval completed, store path resolved"
      lastTransitionTime: "2026-06-05T12:00:00Z"
    - type: ClosureCached
      status: "True"
      reason: CacheVerified
      message: "closure available in binary cache"
      lastTransitionTime: "2026-06-05T12:00:05Z"
    - type: Available
      status: "True"
      reason: ReplicasReady
      message: "3/3 replicas running"
      lastTransitionTime: "2026-06-05T12:00:10Z"
```

Condition types:

| Condition | True | False |
|-----------|------|-------|
| `Evaluated` | Nix eval succeeded | Eval failed or pending |
| `ClosureCached` | NAR available in binary cache | Fetch failed/pending |
| `Available` | >= 1 replica running | No replicas ready |
| `Progressing` | Rollout in progress | Stable |
| `Degraded` | Partial failure | Everything healthy |

If `observedGeneration < metadata.generation`, the controller hasn't
processed the latest spec change. GitOps tools (Argo, Flux) use this
to detect drift.

## Health checks

### CSI DaemonSet

```yaml
livenessProbe:
  httpGet:
    path: /healthz
    port: 9808
  initialDelaySeconds: 10
  periodSeconds: 10
  timeoutSeconds: 3
  failureThreshold: 5
```

The `livenessprobe` sidecar calls `Probe()` on the gRPC socket and
exposes `/healthz` HTTP. If the CSI driver hangs or crashes, kubelet
restarts it.

### Closure integrity

Store paths are content-addressed: the hash is the identity. The CSI
driver can verify integrity by re-hashing the cached NAR and comparing
against the expected NarHash. If corrupted:
1. Evict from local cache
2. Re-fetch from mesh or binary cache
3. Set `Degraded` condition on the NiphasWorkload

## Failure scenario matrix

| Failure | Impact | Recovery |
|---------|--------|----------|
| Single node dies | Pods rescheduled in ~70s. CSI on new node fetches closure: local cache (if previously used) -> mesh peers (LAN) -> binary cache (WAN) | Automatic |
| Binary cache down | No impact on existing workloads (local cache). New closures can still be fetched from mesh peers if any node has them | Automatic (if mesh or local cache available) |
| Binary cache + mesh both down | Existing closures still available on local cache. Only new, never-fetched closures fail | Partial manual |
| niphas-csi DaemonSet pod crashes | kubelet restarts it (liveness probe). Existing mounts survive | Automatic |
| niphas-operator pod dies | New leader elected in ~15s. Existing workloads continue | Automatic |
| niphas-eval pod dies | New workload evaluations fail. Existing workloads unaffected. Deployment restarts pod, eval cache on PVC survives | Automatic |
| Eval hangs (malicious flake) | Per-evaluation timeout (300s) enforced in Rust. Eval cancelled, workload marked Failed | Automatic |
| niphas-mesh pod dies | No P2P fetch from peers. CSI still reads local cache at `/var/lib/niphas/cache/` (shared hostPath). Falls back to binary cache HTTP for uncached paths. Mesh is optional -- CSI works standalone | Automatic |
| NAR corrupted on disk | CSI re-hashes, detects mismatch, evicts, re-fetches | Automatic |
| CRD deleted while pods running | Finalizer holds deletion. Cleanup runs. Then pods terminate | Automatic |
| etcd data loss | CRDs gone. Workloads stop. Must reapply from GitOps | Manual (GitOps redeploy) |
| All nodes die simultaneously | Total cluster failure. Standard K8s DR applies | Manual (cluster restore) |
