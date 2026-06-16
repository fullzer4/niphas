# Security Design

Niphas security architecture: threat model, RBAC, pod security, network policies, and defense in depth.

## Threat Model

| Threat | Mitigation |
|--------|-----------|
| Compromised binary cache | Ed25519 signature verification on every NAR |
| Malicious flakeRef (arbitrary Nix code) | Flake allowlist + eval sandbox (4 layers) |
| Rogue mesh peer | Noise XX mutual auth + PeerId allowlist |
| Privilege escalation from CSI mount | Read-only bind mounts, `MS_NOSUID`, `MS_NODEV` |
| Lateral movement via RBAC | Minimal per-component RBAC, least privilege |

## RBAC

Each component has the minimum RBAC required:

| Component | Scope | Access |
|-----------|-------|--------|
| niphas-operator | ClusterRole | NiphasWorkload CRUD, Deployments, PDBs, Pods, Events |
| niphas-eval | ClusterRole | NiphasWorkload read/update, Events |
| niphas-csi | **None** | Zero K8s API access |
| niphas-mesh | ClusterRole | Read-only NiphasWorkload, Pods, Nodes |

## Pod Security

**CSI DaemonSet** (only privileged component):
- `privileged=true` — required for Bidirectional mount propagation
- `runAsUser=0`
- `readOnlyRootFilesystem=true`

**All other components:**
- `runAsNonRoot=true`, `runAsUser=65534`
- `allowPrivilegeEscalation=false`
- `readOnlyRootFilesystem=true`
- `capabilities: drop: [ALL]`
- `seccompProfile: RuntimeDefault`

## Network Policies

Default deny all. Explicit allow per component:

| Component | Allowed Egress |
|-----------|---------------|
| operator | K8s API (443/6443), DNS |
| eval | Git HTTPS (443), binary cache HTTPS, DNS |
| csi | Binary cache HTTPS, mesh peer (4001), DNS |
| mesh | Mesh peers (4001 TCP+UDP), CSI socket, K8s API, DNS |

## Eval Sandboxing (4 Layers)

1. **Nix evaluator settings:** `sandbox=true`, `restrict_eval=true`, `allow_import_from_derivation=false`, `max_jobs=0`
2. **Flake allowlist:** deny-by-default glob matching before evaluator invoked
3. **Container isolation:** non-root, no capabilities, read-only rootfs, resource limits
4. **Network isolation:** NetworkPolicy allows only git/binary cache HTTPS and DNS

`allow_import_from_derivation=false` is critical — it prevents evaluation from triggering builds.

## Mount Isolation

- Symlink validation: rejects symlinks pointing outside `/nix/store/` or relative escapes
- Mount flags: `MS_BIND | MS_RDONLY | MS_NOSUID | MS_NODEV`
- Target path validation: must be within `/var/lib/kubelet/pods/`

## Mesh Authentication

- libp2p Noise XX handshake (mutual authentication, perfect forward secrecy)
- Closed mesh: PeerId allowlist from K8s API watch
- Keys generated on first boot, stored in K8s Secret per node

## Secrets Management

| Secret | Storage |
|--------|---------|
| Binary cache private keys | External Secrets Operator (Vault, AWS SM) |
| Binary cache public keys | ConfigMap |
| TLS certificates | cert-manager |
| Mesh identity keys | K8s Secret per node |

Kubernetes Secret encryption at rest is required.

## Container Images

- Scratch/distroless base (static musl binaries)
- Read-only root filesystem
- Non-root where possible
- No shell, no package manager
