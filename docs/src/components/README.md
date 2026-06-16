# Components

Niphas is composed of four core components that work together to deliver Nix-built workloads on Kubernetes.

```
                  ┌──────────────┐
  NiphasWorkload  │   Operator   │──── eval webhook ────► Evaluator
  CR ────────────►│  (reconcile) │                           │
                  └──────┬───────┘                     nix eval C FFI
                         │                                   │
                    create Deployment                  binary cache
                    + CSI volume                       closure resolve
                         │
                  ┌──────▼───────┐        ┌─────────────┐
       kubelet ──►│  CSI Driver  │◄──────►│    Mesh     │
                  │ (DaemonSet)  │ socket  │ (DaemonSet) │
                  └──────────────┘        └─────────────┘
                    fetch NAR               P2P distribute
                    bind-mount              gossipsub
```

## Overview

| Component | Type | Role |
|-----------|------|------|
| [Operator](/components/operator) | Deployment | Watches CRDs, reconciles into Deployments/Services/Ingress |
| [Evaluator](/components/eval) | Deployment | Nix evaluation via C FFI, closure resolution |
| [CSI Driver](/components/csi-driver) | DaemonSet | Fetches NARs, mounts `/nix/store` into pods |
| [Mesh](/components/mesh-protocol) | DaemonSet | Optional P2P NAR distribution between nodes |
