# Niphas

> νιφάς – *snowflake*, in Ancient Greek.

**Nix-native platform for Kubernetes.** Run closures, not container images.

> [!WARNING]
> 🚧 **Work in progress.** Niphas is an early-stage experiment. Nothing here is
> stable, production-ready, or even guaranteed to compile. Ideas welcome.

## Why both?

Nix and Kubernetes solve different halves of the deployment problem.
Niphas exists because neither one alone is enough –but together they cover
everything.

**Nix already does better than containers:**

- **Immutability** – every store path is content-addressed by hash, not by
  mutable tags like `latest`.
- **Deduplication** – granular, per-path. No 256-layer limit, no overlayfs
  hacks.
- **Reproducibility** – builds are pure functions. Same input, same output,
  always.
- **Rollback** – atomic, instant. Switch generations, done.
- **Declarativity** – a real functional language, not YAML.

OCI images add nothing here. They wrap a perfectly good closure in layers a
registry can't deduplicate, and throw away everything that makes Nix great.

**Kubernetes does better than anything else:**

- **Scheduling** – distribute workloads across hundreds of nodes.
- **Service discovery** – DNS, Services, Endpoints, out of the box.
- **Health checks & self-healing** – automatic restarts, liveness/readiness
  probes.
- **Horizontal scaling** – HPA, replica sets, cluster autoscaler.
- **Networking** – Ingress, NetworkPolicy, service mesh integration.
- **Observability** – Prometheus, metrics API, audit logs, tracing –the
  entire ecosystem.
- **Failover** – pod rescheduling, node drains, PDB –battle-tested
  resilience at scale.

Most production infrastructure today runs on Kubernetes, and for good reason.
No Nix-only tool replaces this.

**Niphas = Nix for the build/package layer + Kubernetes for everything else.**

The OCI image is the unnecessary middleman. Remove it:

```
git push → CI builds → binary cache → nix eval (in-cluster) → pod mounts the closure
```

CI builds and pushes to a binary cache. The cluster only evaluates
(resolves the store path) and fetches pre-built closures. No builds
happen inside the cluster.

Pods reference store paths directly. Nodes share a deduplicated `/nix/store`.
NiphasWorkload CRDs are delivered by your existing GitOps
(Fleet, Argo CD – untouched). No registry, no Dockerfile, no image tags.
Kubernetes keeps doing what it does best – scheduling, networking, healing,
scaling – on top of a package layer that actually deserves the word
"immutable".

## Components

| Crate | Role |
|---|---|
| `niphas-eval` | Webhook that evaluates flakes via Nix C API (in-process FFI), resolves closures |
| `niphas-operator` | Reconciles the `NiphasWorkload` CRD |
| `niphas-csi` | CSI driver that mounts closures into pods |
| `niphas-mesh` | P2P substitution of store paths between nodes |

100% Rust + Nix. Built on [kube-rs](https://kube.rs).

## Failure handling

Node dies? Pods reschedule in ~70s. The CSI driver on the new node
fetches the closure through a fallback chain:

```
1. local NAR cache (instant, if workload ran here before)
2. mesh peers via libp2p (LAN speed, if mesh enabled)
3. binary cache HTTP (WAN, always available)
```

Each node only caches what its pods actually used -- no proactive
replication, no wasted bandwidth. The mesh is optional: it accelerates
fetches by letting nodes pull NARs from peers instead of the binary cache.

CSI and mesh DaemonSets only run on nodes labeled `niphas.io/store=true`.
Unlabeled nodes have zero Niphas overhead.

The operator creates PDBs, topology spread constraints, and status
conditions following K8s conventions. Infrastructure pods
(CSI, operator) run as `system-cluster-critical` so they survive
resource pressure.

Details: [`docs/RESILIENCE.md`](docs/RESILIENCE.md)

## Security

Every component runs with least privilege. Separate ServiceAccount
and RBAC per component. The CSI driver gets **zero RBAC** (talks to
kubelet via Unix socket only). All other components run under
Restricted Pod Security Standards.

NAR signature verification is mandatory on every fetch (binary cache
or mesh peer). Unverified NARs never reach a pod. Nix eval runs
in-process via Nix C API FFI with sandbox mode, IFD disabled, flake
allowlisting, and dedicated NetworkPolicy.

The mesh is a closed network: libp2p Noise XX provides mutual
authentication with PFS. Only known cluster PeerIds can connect.

Details: [`docs/SECURITY_DESIGN.md`](docs/SECURITY_DESIGN.md)

