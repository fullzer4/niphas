# niphas-csi -- CSI Driver Design

## Why CSI

CSI (Container Storage Interface) is the official K8s storage abstraction.
It is the only approach that:

- Follows K8s storage standards (no hostPath, no node mutation)
- Works under Pod Security Standards Restricted
- Functions on all managed K8s (EKS, GKE, AKS, OpenShift)
- Lets kubelet manage the volume lifecycle correctly
- Supports topology-aware scheduling

## Architecture

niphas-csi is a **Node-only plugin**. No Controller component. No PV/PVC.
It uses **CSI Ephemeral Inline Volumes** (GA since K8s 1.25).

```
pod spec
  volumes:
    - csi:
        driver: niphas.io.csi
        volumeAttributes:
          storePath: "/nix/store/abc123-hello-2.12.1"
          binaryCacheUrl: "https://cache.nixos.org"

kubelet sees driver: niphas.io.csi
  --> connects to /var/lib/kubelet/plugins/niphas.io.csi/csi.sock
  --> calls NodePublishVolume(volume_id, target_path, volume_context)

niphas-csi driver
  --> checks node-local cache at /var/lib/niphas/cache/abc123-hello-2.12.1/
  --> if cache miss: fetches .narinfo from binary cache (HTTP)
  --> downloads and extracts NAR to cache dir
  --> bind-mounts cache dir --> target_path (read-only)
  --> returns success

kubelet starts pod containers with volume mounted

pod deletion
  --> kubelet calls NodeUnpublishVolume
  --> driver unmounts bind mount
  --> cached NAR data stays for reuse by other pods
```

## gRPC Services

CSI v1.12.0 defines 3 gRPC services. niphas-csi only implements Identity + Node.

### Identity Service (3 RPCs, all required)

| RPC | What niphas-csi returns |
|-----|------------------------|
| `GetPluginInfo` | name=`niphas.io.csi`, version from Cargo.toml |
| `GetPluginCapabilities` | empty (no CONTROLLER_SERVICE) |
| `Probe` | ready=true |

### Node Service (4 RPCs implemented, 4 return UNIMPLEMENTED)

| RPC | Status | Purpose |
|-----|--------|---------|
| `NodePublishVolume` | Implemented | Fetch NAR, extract, bind-mount |
| `NodeUnpublishVolume` | Implemented | Unmount bind mount |
| `NodeGetCapabilities` | Implemented | Returns empty capabilities |
| `NodeGetInfo` | Implemented | Returns node hostname |
| `NodeStageVolume` | UNIMPLEMENTED | Not needed (no staging) |
| `NodeUnstageVolume` | UNIMPLEMENTED | Not needed |
| `NodeGetVolumeStats` | UNIMPLEMENTED | Not needed |
| `NodeExpandVolume` | UNIMPLEMENTED | Not needed |

### Controller Service

Not implemented. Not needed for ephemeral inline volumes.

**Total RPCs to implement: 7** (3 Identity + 4 Node).

## Volume Lifecycle

### Pod creation

1. Scheduler places pod on node
2. Kubelet reads pod spec, sees `driver: niphas.io.csi`
3. Kubelet checks `CSIDriver` object: `attachRequired: false`, skips attach
4. Kubelet generates `volume_id` (e.g. `csi-<pod-uid>-<vol-name>`)
5. Kubelet calls `NodePublishVolume` with:
   - `volume_id`
   - `target_path`: `/var/lib/kubelet/pods/<pod-uid>/volumes/kubernetes.io~csi/<vol-name>/mount`
   - `volume_context`: `storePath`, `binaryCacheUrl`, pod metadata
   - `readonly: true`
6. Driver fetches NAR (or hits cache), bind-mounts, returns OK
7. Kubelet starts containers

### Pod deletion

1. Kubelet calls `NodeUnpublishVolume(volume_id, target_path)`
2. Driver unmounts, returns OK
3. Cached data stays on disk for dedup

### Idempotency

Both `NodePublishVolume` and `NodeUnpublishVolume` must be idempotent:
- Publish on already-mounted volume: check mountpoint, return OK
- Unpublish on already-unmounted volume: return OK

## Node-Local Cache and Deduplication

The driver manages a cache at `/var/lib/niphas/cache/` on each node:

```
/var/lib/niphas/cache/
  abc123-hello-2.12.1/         # extracted NAR contents
  def456-glibc-2.38/           # shared dependency
  .tmp-xyz789/                 # in-progress extraction (cleaned on crash)
```

### Dedup guarantees

- **Same path, multiple pods**: first pod triggers download, others hit cache
- **Concurrent requests**: per-path mutex prevents duplicate downloads
- **Atomic population**: extract to `.tmp-*`, then `rename()` into place
- **Crash safety**: incomplete `.tmp-*` dirs are cleaned on driver startup
- **Content-addressed**: hash in path name means cache is always valid (no staleness)

### Garbage collection

Background task periodically:
1. Scans active mounts (ref counting)
2. Evicts unreferenced paths by LRU or disk pressure
3. Never evicts paths with active bind mounts

## Closure Handling

A Nix closure includes a package and all its transitive dependencies.

Closure resolution happens in `niphas-eval` (not the CSI driver). The eval
webhook resolves the full closure by recursively fetching `.narinfo` files
from the binary cache (pure HTTP, no Nix needed) and writes the complete
list to `status.closurePaths` in the CRD.

The operator passes the closure list to the CSI driver via `volumeAttributes`:

```yaml
volumes:
  - name: app-closure
    csi:
      driver: niphas.io.csi
      volumeAttributes:
        closurePaths: "/nix/store/abc123-hello,/nix/store/def456-glibc,..."
        mountMode: "closure"
```

The CSI driver receives the pre-resolved closure list. It does NOT need
to resolve references itself. For each path in the list, it:

1. Checks local cache (`/var/lib/niphas/cache/<basename>/`)
2. If cache miss: fetches from mesh or binary cache
3. After all paths are cached: constructs a merged `/nix/store` view
4. Bind-mounts the merged view at `target_path` (read-only)

This design keeps the CSI driver simple (no `.narinfo` parsing needed for
closure resolution) and avoids depending on binary cache availability
during mount -- the closure list is already known.

## Serving on Unix Domain Socket

The driver listens on a UDS, not TCP. kubelet communicates with CSI drivers
exclusively via Unix sockets.

```
/var/lib/kubelet/plugins/niphas.io.csi/csi.sock   <-- driver socket
/var/lib/kubelet/plugins_registry/                 <-- registrar creates reg socket here
```

The `node-driver-registrar` sidecar:
1. Connects to the driver socket
2. Calls `GetPluginInfo` + `NodeGetInfo`
3. Creates a registration socket in `/var/lib/kubelet/plugins_registry/`
4. kubelet's plugin watcher discovers the driver

## CSIDriver K8s Object

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
  requiresRepublish: false
  seLinuxMount: false
```

| Field | Value | Reason |
|-------|-------|--------|
| `attachRequired` | false | No controller, kubelet calls Node directly |
| `podInfoOnMount` | true | Driver receives pod name/namespace/uid in volume_context |
| `volumeLifecycleModes` | Ephemeral | Inline volumes only (for now) |
| `fsGroupPolicy` | None | Nix store paths are immutable, no permission changes |

## DaemonSet Deployment

```yaml
apiVersion: apps/v1
kind: DaemonSet
metadata:
  name: niphas-csi-node
  namespace: niphas-system
spec:
  selector:
    matchLabels:
      app: niphas-csi-node
  template:
    metadata:
      labels:
        app: niphas-csi-node
    spec:
      serviceAccountName: niphas-csi-node
      containers:
        - name: niphas-csi
          image: ghcr.io/fullzer4/niphas-csi:latest
          securityContext:
            privileged: true
          env:
            - name: NODE_NAME
              valueFrom:
                fieldRef:
                  fieldPath: spec.nodeName
          volumeMounts:
            - name: socket-dir
              mountPath: /csi
            - name: pods-mount-dir
              mountPath: /var/lib/kubelet/pods
              mountPropagation: Bidirectional
            - name: niphas-cache
              mountPath: /var/lib/niphas/cache

        - name: node-driver-registrar
          image: registry.k8s.io/sig-storage/csi-node-driver-registrar:v2.13.0
          args:
            - "--csi-address=/csi/csi.sock"
            - "--kubelet-registration-path=/var/lib/kubelet/plugins/niphas.io.csi/csi.sock"
          volumeMounts:
            - name: socket-dir
              mountPath: /csi
            - name: registration-dir
              mountPath: /registration

        - name: liveness-probe
          image: registry.k8s.io/sig-storage/livenessprobe:v2.14.0
          args:
            - "--csi-address=/csi/csi.sock"
            - "--health-port=9898"
          volumeMounts:
            - name: socket-dir
              mountPath: /csi

      volumes:
        - name: socket-dir
          hostPath:
            path: /var/lib/kubelet/plugins/niphas.io.csi/
            type: DirectoryOrCreate
        - name: pods-mount-dir
          hostPath:
            path: /var/lib/kubelet/pods
            type: Directory
        - name: registration-dir
          hostPath:
            path: /var/lib/kubelet/plugins_registry/
            type: Directory
        - name: niphas-cache
          hostPath:
            path: /var/lib/niphas/cache
            type: DirectoryOrCreate
```

### Key volume details

| Volume | hostPath | Why |
|--------|----------|-----|
| `socket-dir` | `/var/lib/kubelet/plugins/niphas.io.csi/` | Driver socket, shared with registrar |
| `pods-mount-dir` | `/var/lib/kubelet/pods` | Where kubelet expects bind mounts. **Must have `mountPropagation: Bidirectional`** |
| `registration-dir` | `/var/lib/kubelet/plugins_registry/` | Registrar creates reg socket here for kubelet discovery |
| `niphas-cache` | `/var/lib/niphas/cache` | Node-local NAR cache, persists across DaemonSet restarts |

### Why the DaemonSet uses hostPath but consumer pods don't

The CSI DaemonSet needs hostPath to:
- Place the Unix socket where kubelet expects it
- Access kubelet's pod mount directory
- Maintain a persistent cache

But **consumer pods don't use hostPath**. They use standard CSI inline volumes.
The CSI abstraction exists precisely to give privileged access to the driver
while keeping consumer pods unprivileged.

## NAR Fetching

The CSI driver fetches individual NARs via the fallback chain
(see [`RESILIENCE.md`](RESILIENCE.md)):

1. Local cache (`/var/lib/niphas/cache/<basename>/`)
2. Mesh P2P via Unix socket (`/var/run/niphas/mesh.sock`)
3. Binary cache HTTP (direct)

For each store path in the closure list (received via `volumeAttributes`):

1. Fetch `.narinfo` from binary cache
2. Verify Ed25519 signature
3. Download compressed NAR (`.nar.zst`)
4. Verify `FileHash` (compressed)
5. Decompress
6. Verify `NarHash` (uncompressed, computed inline during extraction)
7. Extract to temp dir with safety checks (CVE-2024-45593 mitigations)
8. Atomic rename to cache dir

All NAR format parsing, hash verification, and signature checking is
implemented in-house in `niphas-core`. See [`NIX_WIRE.md`](NIX_WIRE.md)
for the complete specification.

## Sidecars

| Sidecar | Required | Image | Purpose |
|---------|----------|-------|---------|
| node-driver-registrar | Yes | `registry.k8s.io/sig-storage/csi-node-driver-registrar:v2.13.0` | Registers driver with kubelet |
| livenessprobe | Recommended | `registry.k8s.io/sig-storage/livenessprobe:v2.14.0` | Health monitoring via `Probe` RPC |
| csi-provisioner | No | - | Only for PV/PVC (not needed for ephemeral) |
| csi-attacher | No | - | Only for `attachRequired: true` |

## Proto File

Vendor CSI v1.12.0 proto from:
```
https://github.com/container-storage-interface/spec/blob/v1.12.0/csi.proto
```

Place at `proto/csi/v1/csi.proto`. Build with `tonic-build` in `build.rs`.

Proto depends on `google/protobuf/{descriptor,timestamp,wrappers}.proto`,
which prost/tonic-build bundle automatically.
