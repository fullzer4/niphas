---
layout: home

hero:
  name: niphas
  text: Nix-native platform for Kubernetes
  tagline: "νιφάς — declarative workloads, evaluated and delivered by Nix"
  actions:
    - theme: brand
      text: Get Started
      link: /architecture/
    - theme: alt
      text: GitHub
      link: https://github.com/fullzer4/niphas

features:
  - title: Nix Evaluator
    details: Evaluates flake references into derivations and streams build artifacts directly into the cluster.
  - title: CSI Driver
    details: Mounts Nix store paths as volumes, enabling transparent access to build outputs for workloads.
  - title: Mesh Protocol
    details: Coordinates operator, evaluator, and CSI driver through a resilient gRPC mesh.
---
