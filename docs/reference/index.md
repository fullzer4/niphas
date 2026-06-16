# Reference

Technical reference documentation for niphas internals, APIs, and operational concerns.

## API & Formats

| Document | Description |
|----------|-------------|
| [CRD Reference](/reference/crd-reference) | Complete `NiphasWorkload` API spec — fields, status, conditions, validation |
| [Nix Wire Format](/reference/nix-wire) | NAR format, `.narinfo`, hashing, Ed25519 signatures, binary cache protocol |

## Operations

| Document | Description |
|----------|-------------|
| [Security Design](/reference/security-design) | Threat model, RBAC, pod security, network policies, eval sandboxing |
| [Resilience](/reference/resilience) | Failure handling, fallback chains, PDB, topology spread, priority classes |
| [Testing](/reference/testing) | Test strategy, sans-IO patterns, trait injection, tiers |

## Other

| Document | Description |
|----------|-------------|
| [Security Policy](/reference/security) | Vulnerability reporting and disclosure policy |
| [Known Issues](/reference/problem) | Tracked problems and planned fixes |
