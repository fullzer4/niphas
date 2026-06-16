# Mesh Protocol Design

Optional P2P distribution of NARs between nodes via libp2p. An optimization for scale, speed, and resilience.

## Overview

The mesh is a DaemonSet providing peer-to-peer NAR distribution across cluster nodes. It's optional — the system works without it, falling back to binary cache HTTP.

**When it helps:**
- Scale: 100+ nodes pulling the same NAR
- Speed: LAN transfer vs WAN binary cache
- Resilience: if binary cache is temporarily down

## Architecture

Every mesh pod is equal — no leader election needed. NARs are content-addressed and immutable, so any peer can serve any cached NAR.

Deployed only on nodes with both `niphas.io/store=true` and `niphas.io/mesh=true` labels. Resource usage: ~40–60 MB RAM when idle.

## libp2p Stack

| Layer | Choice |
|-------|--------|
| Transport | TCP + QUIC |
| Encryption | Noise XX (mutual auth, PFS) |
| Multiplexing | Yamux |
| Discovery | DNS (Headless Service) + K8s API watch |
| Messaging | Gossipsub |
| Data transfer | libp2p streams |

Port: 4001 (TCP + UDP)

## Protocol Messages

### Gossipsub (topic: `niphas/nar/v1`)

- **Have** — announces a NAR is available locally (`nar_hash`, `store_path`, `nar_size`)
- **Evicted** — announces a NAR was garbage-collected (`nar_hash`)

### Control Plane (CBOR)

- **HasNar** / **QueryNarInfo** — point-to-point queries
- Responses: `Available`, `NarSize`, `NarInfo`, `NotFound`, `Busy`

### Data Plane (libp2p streams)

- `TransferHeader` (48 bytes fixed) + raw compressed NAR bytes
- For NARs larger than 64 MB: parallel range download (swarming) from multiple peers

## NAR Index

Each node maintains a local `HashMap` — "who has NAR X?" — populated entirely from gossipsub messages. O(1) lookup, ~240 KB for 3000 store paths.

## Peer Discovery

1. **Bootstrap:** Headless Service DNS resolution
2. **Dynamic:** K8s API watch for real-time membership changes + node labels

## CSI Integration

The CSI driver communicates with the mesh via a Unix socket at `/var/run/niphas/mesh.sock` using JSON-RPC:

```
CSI ──FetchNar──► mesh socket
                    │
              NAR index lookup
                    │
              fetch from best peer
              (same zone preferred)
                    │
              verify + announce Have
                    │
              respond Fetched
```

If mesh is unavailable, CSI falls back to binary cache HTTP transparently.

## Cache Management

- **Per-node lazy caching** — each node only caches NARs for pods it runs
- No proactive replication — minimal bandwidth overhead
- **GC:** high watermark 85% → evict LRU unmounted NARs → low watermark 75%

## Authentication

- Noise XX handshake provides mutual authentication and perfect forward secrecy
- Closed mesh: PeerId allowlist populated from K8s API watch
- Keys generated on first boot, stored in K8s Secret per node

## Bandwidth Control

- Per-peer rate limiting via `governor` crate
- Priority queue: CSI mount requests take priority over peer-serving requests
- Configurable connection limits
