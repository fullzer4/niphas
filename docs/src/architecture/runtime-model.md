# Runtime Model

How Nix-built binaries run inside Kubernetes without traditional container images.

## The Core Insight

Nix closures are more self-contained than OCI images. Every binary carries absolute `/nix/store` paths for all dependencies — the dynamic linker, shared libraries, and runtime data are all pinned.

## How Nix Makes Binaries Self-Contained

- **`patchelf`** rewrites `PT_INTERP` (dynamic linker) and `DT_RUNPATH` (shared lib search) to `/nix/store` paths
- **`patchShebangs`** fixes script interpreters (`#!/nix/store/...`)
- **Closures** are transitively complete — every referenced store path is included
- **Static builds** (musl) have no `PT_INTERP` or `DT_RUNPATH` at all

## How It Runs in Kubernetes

**Problem:** Kubernetes requires an `image` field on every container.

**Solution:** A stub image (`niphas-runner`, ~1 MB) provides the minimal filesystem, while the actual binary comes from a CSI volume mount.

The stub provides only:
- `/etc/passwd`, `/etc/group`, `/etc/nsswitch.conf`
- `/tmp`

The CSI driver mounts the Nix closure at `/nix/store`, making the binary and all its dependencies available.

### Execution Sequence

1. Kubelet calls `NodePublishVolume` on the CSI driver
2. CSI checks local cache → mesh peers → binary cache HTTP
3. NAR is fetched, verified (Ed25519 + hash), and extracted
4. Bind-mount at the pod's volume target
5. Container starts with the Nix binary as entrypoint
6. Dynamic linker loads from `/nix/store` paths — no host dependencies needed

Kubelet automatically provides `/proc`, `/sys`, `/dev`, `/etc/resolv.conf`, `/etc/hosts`.

## Edge Cases

| Scenario | Solution |
|----------|----------|
| Programs hardcoding `/bin/sh` | Fix at package level or symlink in stub |
| GPU workloads | Standard NVIDIA device plugin pattern |
| `setuid` binaries | CSI mount uses `MS_NOSUID` |
| `dlopen` plugins | `patchelf`, `wrapProgram`, or CRD env vars |

## Comparison with Traditional Containers

| Aspect | Traditional OCI | Niphas |
|--------|----------------|--------|
| Build | Dockerfile → layers | Nix derivation → NAR |
| Size | 50MB–1GB typical | Exact closure size |
| Deduplication | Per-layer | Per-store-path |
| Reproducibility | Best-effort | Bit-for-bit |
| Registry | Required | Binary cache (optional) |
| Update granularity | Full layer | Individual store paths |
