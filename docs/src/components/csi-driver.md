# CSI Driver Design

The niphas-csi driver mounts Nix store paths as ephemeral volumes inside Kubernetes pods.

## Why CSI

- Official Kubernetes storage abstraction
- Works under Pod Security Standards Restricted (consumer pods stay unprivileged)
- Functions on all managed Kubernetes providers
- CSI ephemeral inline volumes are GA since Kubernetes 1.25

## Architecture

Node-only plugin — no Controller service. The driver runs as a DaemonSet on nodes labeled `niphas.io/store=true`.

### gRPC Services

**Identity Service:**
- `GetPluginInfo` — returns driver name and version
- `GetPluginCapabilities` — no optional capabilities
- `Probe` — readiness check

**Node Service:**
- `NodePublishVolume` — fetch NAR and bind-mount at target
- `NodeUnpublishVolume` — unmount and clean up
- `NodeGetCapabilities` — reports supported features
- `NodeGetInfo` — returns node ID

No Controller service is needed — everything is node-local.

## Volume Lifecycle

1. **Pod creation** → kubelet calls `NodePublishVolume`
2. Driver checks local cache → mesh peers → binary cache HTTP
3. NAR fetched, verified (Ed25519 signatures + hash), extracted
4. Bind-mount at target path
5. **Pod deletion** → kubelet calls `NodeUnpublishVolume`
6. Unmount, cached data persists for future pods

Both publish and unpublish are idempotent.

## NAR Fetch Chain

```
local cache ──miss──► mesh P2P ──miss──► binary cache HTTP ──miss──► gRPC Unavailable
  (instant)           (LAN, 5s)          (WAN, HTTPS)               (kubelet retries)
```

For each NAR:

1. Fetch `.narinfo` → verify Ed25519 signature
2. Download compressed NAR → verify `FileHash`
3. Decompress → verify `NarHash` (inline streaming)
4. Extract to `.tmp-*` → atomic rename (CVE-2024-45593 mitigations)

**Unverified NAR never reaches a pod.**

## Node-Local Cache

Location: `/var/lib/niphas/cache/`

- Content-addressed storage
- Atomic population (`.tmp-*` then rename)
- Crash-safe — incomplete downloads are cleaned up
- LRU garbage collection (high watermark 85%, low watermark 75%)

## Closure Handling

The eval webhook pre-resolves the full closure and passes it via `volumeAttributes.closurePaths`. The CSI driver fetches individual NARs — it doesn't need `.narinfo` parsing for closure resolution.

## DaemonSet

The CSI DaemonSet requires:

- **Privileged mode** — needed for `Bidirectional` mount propagation
- `hostPath` volumes for socket registration and NAR cache
- Sidecars: `node-driver-registrar` (required), `livenessprobe` (recommended)

Consumer pods remain fully unprivileged — the CSI abstraction layer handles the privilege boundary.

## CSIDriver Object

```yaml
apiVersion: storage.k8s.io/v1
kind: CSIDriver
metadata:
  name: niphas.io.csi
spec:
  attachRequired: false
  podInfoOnMount: true
  volumeLifecycleModes:
    - Ephemeral
  fsGroupPolicy: None
```
