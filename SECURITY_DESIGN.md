# Niphas -- Security Design

## Threat model

Niphas runs privileged infrastructure on K8s clusters. Attack surface:

1. **Compromised binary cache** serves malicious NARs with valid signatures
2. **Malicious flakeRef** in a NiphasWorkload CRD runs arbitrary Nix code
3. **Rogue mesh peer** joins the P2P network and injects bad data
4. **Privilege escalation** from CSI driver mount operations
5. **Lateral movement** through overly permissive RBAC or network access

Every component follows least privilege. Each gets its own ServiceAccount,
RBAC, and NetworkPolicy.

## RBAC

### niphas-operator

Broadest permissions. Reconciles CRDs, creates Deployments and PDBs.

```yaml
apiVersion: v1
kind: ServiceAccount
metadata:
  name: niphas-operator
  namespace: niphas-system
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: niphas-operator
rules:
  - apiGroups: ["niphas.io"]
    resources: ["niphasworkloads"]
    verbs: ["get", "list", "watch", "create", "update", "patch", "delete"]
  - apiGroups: ["niphas.io"]
    resources: ["niphasworkloads/status"]
    verbs: ["get", "update", "patch"]
  - apiGroups: ["niphas.io"]
    resources: ["niphasworkloads/finalizers"]
    verbs: ["update"]
  - apiGroups: ["apps"]
    resources: ["deployments"]
    verbs: ["get", "list", "watch", "create", "update", "patch", "delete"]
  - apiGroups: ["policy"]
    resources: ["poddisruptionbudgets"]
    verbs: ["get", "list", "watch", "create", "update", "patch", "delete"]
  - apiGroups: [""]
    resources: ["events"]
    verbs: ["create", "patch"]
  - apiGroups: [""]
    resources: ["pods"]
    verbs: ["get", "list", "watch"]
---
# Leader election (namespace-scoped)
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: niphas-operator-leader-election
  namespace: niphas-system
rules:
  - apiGroups: ["coordination.k8s.io"]
    resources: ["leases"]
    verbs: ["get", "list", "watch", "create", "update", "patch", "delete"]
```

### niphas-csi

**Zero RBAC.** Communicates with kubelet over Unix domain socket only.
Never contacts the K8s API.

```yaml
apiVersion: v1
kind: ServiceAccount
metadata:
  name: niphas-csi-node
  namespace: niphas-system
  annotations:
    automountServiceAccountToken: "false"
```

### niphas-mesh

Read-only access to discover peers and read closure info.

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: niphas-mesh
rules:
  - apiGroups: ["niphas.io"]
    resources: ["niphasworkloads"]
    verbs: ["get", "list", "watch"]
  - apiGroups: [""]
    resources: ["pods"]
    verbs: ["get", "list", "watch"]
  - apiGroups: [""]
    resources: ["nodes"]
    verbs: ["get", "list", "watch"]
```

### niphas-eval

Evaluates flakes via Nix C API (in-process, no Jobs), updates workload status.

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: niphas-eval
rules:
  - apiGroups: ["niphas.io"]
    resources: ["niphasworkloads"]
    verbs: ["get", "list", "watch"]
  - apiGroups: ["niphas.io"]
    resources: ["niphasworkloads/status"]
    verbs: ["get", "update", "patch"]
  - apiGroups: [""]
    resources: ["events"]
    verbs: ["create", "patch"]
```

No batch/jobs or pods RBAC needed -- evaluation happens in-process
via C FFI, not in separate Jobs.

## Pod Security

### CSI DaemonSet (needs mount privileges)

```yaml
securityContext:
  privileged: true
  runAsUser: 0
  readOnlyRootFilesystem: true
```

The CSI driver requires `privileged: true` for bind mount operations
with `mountPropagation: Bidirectional`. This is the same approach used
by every production CSI driver (EBS CSI, GCE PD, Ceph, Longhorn).

`CAP_SYS_ADMIN` alone is insufficient because Bidirectional mount
propagation requires the `privileged` flag in most container runtimes
(containerd, CRI-O). Attempting `CAP_SYS_ADMIN` without `privileged`
fails silently on many K8s distributions.

The blast radius is contained: the CSI driver has zero RBAC (no K8s
API access), read-only rootfs, and only communicates via Unix socket
with kubelet.

### All other components (Restricted PSS)

```yaml
securityContext:
  runAsNonRoot: true
  runAsUser: 65534
  runAsGroup: 65534
  seccompProfile:
    type: RuntimeDefault
containers:
  - securityContext:
      allowPrivilegeEscalation: false
      readOnlyRootFilesystem: true
      capabilities:
        drop: ["ALL"]
```

### Namespace enforcement

```yaml
apiVersion: v1
kind: Namespace
metadata:
  name: niphas-system
  labels:
    pod-security.kubernetes.io/enforce: baseline
    pod-security.kubernetes.io/enforce-version: latest
    pod-security.kubernetes.io/warn: restricted
    pod-security.kubernetes.io/warn-version: latest
    pod-security.kubernetes.io/audit: restricted
    pod-security.kubernetes.io/audit-version: latest
```

Baseline enforced (CSI needs it). Restricted on warn/audit to track
which pods need elevation.

## NetworkPolicy

Default deny for the namespace, then explicit allow per component.

```yaml
# Default deny all
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: default-deny-all
  namespace: niphas-system
spec:
  podSelector: {}
  policyTypes: [Ingress, Egress]
```

### Per-component network access

| Component | Ingress | Egress |
|-----------|---------|--------|
| operator | health probes (monitoring) | K8s API (443/6443), DNS |
| csi | liveness probe (kubelet) | binary cache HTTPS, mesh peer (4001), DNS |
| mesh | mesh peers (4001 TCP+UDP), CSI peer requests, monitoring | mesh peers (4001), K8s API, DNS |
| eval | webhook endpoint (8443 from operator) | git HTTPS (443), binary cache HTTPS, DNS |

Each component has its own NetworkPolicy. The eval pod only needs
outbound HTTPS for fetching flake sources (git) and resolving closures
(binary cache .narinfo). No K8s API access needed beyond the minimal
RBAC for status updates.

## NAR signature verification

Every NAR fetched from any source (binary cache or mesh peer) must
be verified before extraction or use.

### How Nix signatures work

`.narinfo` contains:
```
Sig: <key-name>:<base64-ed25519-signature>
```

Signature covers the fingerprint:
```
1;<store-path>;<nar-hash>;<nar-size>;<references>
```

### Verification in niphas-csi

```rust
use ed25519_dalek::{Signature, VerifyingKey, Verifier};

fn verify_narinfo(
    narinfo: &NarInfo,
    trusted_keys: &[TrustedKey],
) -> Result<(), NarVerificationError> {
    let fingerprint = format!(
        "1;{};{};{};{}",
        narinfo.store_path,
        narinfo.nar_hash,
        narinfo.nar_size,
        narinfo.references.join(",")
    );

    for sig in &narinfo.signatures {
        for key in trusted_keys {
            if sig.key_name == key.name {
                let sig_bytes = base64_decode(&sig.signature)?;
                let signature = Signature::from_bytes(&sig_bytes.try_into()?);
                if key.pubkey.verify(fingerprint.as_bytes(), &signature).is_ok() {
                    return Ok(());
                }
            }
        }
    }

    Err(NarVerificationError::NoTrustedSignature)
}
```

### On verification failure

1. Reject the NAR. Never extract or cache it.
2. Log warning event with store path, cache URL, signatures present.
3. Set `Degraded` condition with reason `NarSignatureVerificationFailed`.
4. Fall back to next source in fetch chain.
5. If all sources fail: return gRPC `Unavailable`, kubelet retries.

**Invariant: an unverified NAR never reaches a pod.**

Mesh-fetched NARs go through the same verification. The mesh is a
transport layer, not a trust layer.

### Trusted keys config

```yaml
binaryCaches:
  - url: "https://cache.nixos.org"
    publicKey: "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY="
    requireSignature: true
  - url: "https://cache.company.com"
    publicKey: "cache.company.com-1:..."
    requireSignature: true
```

## Eval sandboxing (highest risk)

Nix evaluation executes arbitrary Nix code. A malicious flake can
read files, make network requests, and with IFD trigger builds.

niphas-eval calls the Nix evaluator via C FFI (in-process, no Jobs).
The evaluator runs inside the niphas-eval Deployment pod with
multiple layers of defense.

### Defense in depth (4 layers)

**Layer 1: Nix evaluator settings (via C API)**

```rust
// Applied before every evaluation via nix-bindings-rust
settings.sandbox = true;
settings.restrict_eval = true;
settings.allowed_uris = vec![
    "https://github.com",
    "https://cache.nixos.org",
];
settings.allow_import_from_derivation = false;  // critical
settings.max_jobs = 0;                          // eval only, no builds
```

`allow-import-from-derivation = false` is critical: blocks IFD which
would allow executing derivations during eval.

`max-jobs = 0` ensures no builds can happen even if IFD is somehow
bypassed.

**Layer 2: Flake allowlist (deny-by-default)**

```yaml
allowedFlakeOrigins:
  - "github:myorg/*"
  - "github:nixos/nixpkgs"
```

The eval webhook rejects any flakeRef not matching the allowlist
before the evaluator is invoked. Deny-by-default: if a flakeRef
does not match any entry, it is rejected.

**Layer 3: Container isolation**

```yaml
# niphas-eval Deployment pod
securityContext:
  runAsNonRoot: true
  runAsUser: 65534
  seccompProfile:
    type: RuntimeDefault
containers:
  - securityContext:
      allowPrivilegeEscalation: false
      readOnlyRootFilesystem: true
      capabilities:
        drop: ["ALL"]
    resources:
      limits:
        cpu: "2"
        memory: "4Gi"
```

The eval process runs as non-root, no capabilities, read-only rootfs.
The Nix store for eval is mounted on a writable volume (PVC or hostPath)
at `/nix/store` for caching evaluated derivations.

Per-evaluation timeout enforced in Rust (e.g. `tokio::time::timeout(300s)`).

**Layer 4: Network isolation**

```yaml
# NetworkPolicy for niphas-eval pod
spec:
  podSelector:
    matchLabels:
      app: niphas-eval
  policyTypes: [Ingress, Egress]
  ingress:
    - from:
        - podSelector:
            matchLabels:
              app: niphas-operator
      ports:
        - port: 8443    # webhook endpoint
  egress:
    - to: []             # git HTTPS (443) + DNS (53)
      ports:
        - port: 443
          protocol: TCP
        - port: 53
          protocol: UDP
        - port: 53
          protocol: TCP
```

niphas-eval can reach git (HTTPS) and DNS only. Cannot reach K8s API
(no RBAC for sensitive resources), cannot reach other pods except
via the webhook endpoint.

## Mount isolation

### Symlink attack prevention

NAR extraction validates every entry:

```rust
fn validate_nar_entry(entry: &NarEntry) -> Result<()> {
    match entry {
        NarEntry::Symlink { target } => {
            // Reject symlinks pointing outside /nix/store/
            if !target.starts_with("/nix/store/") && !is_relative_within_store(target) {
                return Err(NarExtractionError::SymlinkEscape);
            }
        }
        NarEntry::Directory { name } => {
            // Reject path traversal
            if name.contains("..") || name.contains('/') {
                return Err(NarExtractionError::PathTraversal);
            }
        }
        _ => {}
    }
    Ok(())
}
```

### Mount flags

All bind mounts use:
- `MS_BIND | MS_RDONLY` (read-only)
- `MS_NOSUID` (no setuid binaries)
- `MS_NODEV` (no device nodes)

```rust
mount(
    Some(cache_path), target_path,
    None::<&str>,
    MsFlags::MS_BIND | MsFlags::MS_RDONLY | MsFlags::MS_NOSUID | MsFlags::MS_NODEV,
    None::<&str>,
)?;
```

### Target path validation

Before mounting, verify target is within kubelet's pod directory:

```rust
fn validate_target_path(target: &Path) -> Result<()> {
    let canonical = target.canonicalize()?;
    if !canonical.starts_with("/var/lib/kubelet/pods/") {
        return Err(CsiError::InvalidTargetPath(canonical));
    }
    Ok(())
}
```

Prevents a compromised kubelet from tricking the CSI driver into
mounting at an arbitrary host path.

## Mesh authentication

libp2p Noise XX handshake provides:
- **Mutual authentication**: both peers prove identity key possession
- **Perfect forward secrecy**: ephemeral Curve25519 keys per session
- **Encryption**: ChaCha20-Poly1305 post-handshake

### Closed mesh

Only cluster members can participate. Each mesh pod registers its
PeerId via K8s API (Pod annotation). Peers reject connections from
unknown PeerIds.

```rust
let allowed_peers: HashSet<PeerId> = discover_cluster_peers().await?;

swarm.behaviour_mut().set_connection_handler(move |peer_id| {
    if !allowed_peers.contains(peer_id) {
        return Err(ConnectionDenied::new("peer not in cluster"));
    }
    Ok(())
});
```

### CSI-to-mesh communication

Uses Unix domain socket at `/var/run/niphas/mesh.sock` for same-node
communication. No TLS needed (inherently node-scoped, no network
exposure).

## Secrets management

| Secret | Storage |
|--------|---------|
| Binary cache signing keys (Ed25519 private) | External Secrets Operator (Vault, AWS SM) |
| Binary cache trusted public keys | ConfigMap (public data) |
| Webhook TLS certs (eval) | cert-manager with internal CA |
| Mesh identity keys (libp2p Ed25519) | K8s Secret per node, rotated |
| Git SSH keys (private flake repos) | K8s Secret type `kubernetes.io/ssh-auth` |

Encryption at rest must be enabled for K8s Secrets (`EncryptionConfiguration`).

## Container images

| Component | Base image | User | Root FS |
|-----------|-----------|------|---------|
| operator | `scratch` (static musl binary) | 65534 | read-only |
| csi | distroless or `scratch` + musl | 0 (root, for mount) | read-only |
| mesh | `scratch` (static musl binary) | 65534 | read-only |
| eval | Nix-based image (contains Nix libs for C FFI) | 65534 | read-only + PVC for /nix/store |

Built via `dockerTools.buildLayeredImage` in Nix (no shell, no pkg manager).
The eval image includes `libexpr-c`, `libstore-c`, `libflake-c` shared
libraries for the Nix C API, but no `nix` CLI binary.

## Audit logging

K8s API server audit policy:

| Resource | Verbs | Level |
|----------|-------|-------|
| niphasworkloads | create, update, patch, delete | RequestResponse |
| niphasworkloads | get, list, watch | Metadata |
| niphasworkloads/status | update, patch | Request |
| secrets (niphas-system) | all | Metadata |

Each component also emits structured logs:
- **operator**: reconciliation actions + results
- **eval**: flake evaluation requests + pass/fail
- **csi**: NAR fetch source + signature verification result
- **mesh**: peer connections + NAR transfers

## Summary

| Area | Approach |
|------|----------|
| RBAC | Separate SA per component. CSI gets zero RBAC. |
| PSS | CSI: privileged (required for mount propagation). Others: Restricted. |
| Network | Default deny + explicit allow per component. |
| NAR integrity | Mandatory Ed25519 verification on all sources. |
| Eval sandbox | 4 layers: Nix C API settings (IFD=false), allowlist, container isolation, network policy. |
| Mount safety | Symlink validation, read-only + nosuid + nodev, target path validation. |
| Mesh auth | libp2p Noise XX, closed mesh (PeerId allowlist). |
| Secrets | External Secrets Operator for private keys. cert-manager for TLS. |
| Images | scratch/distroless, read-only rootfs, non-root where possible. |
| Audit | K8s audit policy + structured component logs. |
