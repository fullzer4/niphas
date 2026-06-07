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
git push → nix eval → build (in-cluster) → binary cache → pod mounts the closure
```

Pods reference store paths directly. Nodes share a deduplicated `/nix/store`.
Manifests are rendered from Nix and delivered by your existing GitOps
(Fleet, Argo CD – untouched). No registry, no Dockerfile, no image tags.
Kubernetes keeps doing what it does best – scheduling, networking, healing,
scaling – on top of a package layer that actually deserves the word
"immutable".

## Components

| Crate | Role |
|---|---|
| `niphas-eval` | Webhook → evaluates flakes, schedules builds as Jobs |
| `niphas-operator` | Reconciles the `NiphasWorkload` CRD |
| `niphas-csi` | CSI driver that mounts closures into pods |
| `niphas-mesh` | P2P substitution of store paths between nodes |

100% Rust + Nix. Built on [kube-rs](https://kube.rs),

