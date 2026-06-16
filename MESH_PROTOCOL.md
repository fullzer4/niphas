# Niphas -- Mesh Protocol Design

## Overview

niphas-mesh is an **optional** DaemonSet that runs on nodes labeled with
`niphas.io/store=true`. It provides P2P distribution of Nix store paths
(NARs) between Niphas-enabled nodes, accelerating closure fetching via
LAN-speed peer transfers instead of WAN binary cache HTTP.

The mesh is NOT required. Without it, the CSI driver fetches directly from
binary cache HTTP. The mesh is an optimization for:

- **Scale**: 100+ nodes fetching the same NAR simultaneously would overwhelm
  a single binary cache server. P2P distributes the load.
- **Speed**: LAN peer fetch (~1 GB/s) vs WAN binary cache (~100 MB/s).
- **Resilience**: If the binary cache goes down, peers serve cached NARs.

**No full replication.** Each node only caches the NARs its pods actually
use. The mesh enables fast peer-to-peer fetch on cache miss, not cluster-wide
synchronization.

## Selective Deployment

### Deployer controls via node labels

```bash
# Mark nodes that should run Niphas workloads
kubectl label node worker-1 niphas.io/store=true
kubectl label node worker-2 niphas.io/store=true

# Nodes without the label: zero Niphas overhead
# worker-3 through worker-100: no CSI, no mesh, no cache
```

### DaemonSet nodeSelector

```yaml
# CSI DaemonSet -- only on labeled nodes
apiVersion: apps/v1
kind: DaemonSet
metadata:
  name: niphas-csi
  namespace: niphas-system
spec:
  selector:
    matchLabels:
      app: niphas-csi
  template:
    spec:
      nodeSelector:
        niphas.io/store: "true"
      # ...

---
# Mesh DaemonSet -- only on labeled nodes, optional
apiVersion: apps/v1
kind: DaemonSet
metadata:
  name: niphas-mesh
  namespace: niphas-system
spec:
  selector:
    matchLabels:
      app: niphas-mesh
  template:
    spec:
      nodeSelector:
        niphas.io/store: "true"
      # ...
```

### Resource usage

| Node type | CSI pod | Mesh pod | Cache | RAM overhead |
|-----------|---------|----------|-------|-------------|
| `niphas.io/store=true` | Running (~10 MB idle) | Running (~30-50 MB idle) | Grows with usage | ~40-60 MB |
| No label | None | None | None | 0 |

For comparison: calico-node uses 150-256 MB, Cilium uses 330-430 MB.

### Mesh is optional

```yaml
# Helm values
mesh:
  enabled: true          # false = CSI-only, binary cache HTTP
  nodeSelector:
    niphas.io/store: "true"
  resources:
    requests:
      cpu: "50m"
      memory: "64Mi"
    limits:
      cpu: "500m"
      memory: "256Mi"
```

When `mesh.enabled: false`, the CSI driver fetches exclusively from binary
cache HTTP. For small clusters with a fast self-hosted binary cache
(Attic/harmonia on the same LAN), this is sufficient.

## Architecture

```
              Nodes with niphas.io/store=true
   +--------------------------------------------------+
   |                                                    |
   |  +------------------+    +---------------------+  |
   |  | niphas-csi       |    | niphas-mesh         |  |
   |  | (DaemonSet)      |    | (DaemonSet,optional)|  |
   |  |                  |    |                     |  |
   |  | NodePublishVolume|    | Gossipsub           |  |
   |  | NodeUnpublish    |    | Request-Response    |  |
   |  | NAR verification |    | libp2p_stream       |  |
   |  +--------+---------+    +----------+----------+  |
   |           |                         |              |
   |           +-------+   +------------+              |
   |                   |   |                            |
   |           +-------v---v--------+                   |
   |           | /var/lib/niphas/   |                   |
   |           |   cache/           |                   |
   |           | (hostPath, shared) |                   |
   |           +--------------------+                   |
   +--------------------------------------------------+

              Nodes WITHOUT label
   +--------------------------------------------------+
   |  No Niphas pods. Zero overhead.                   |
   +--------------------------------------------------+
```

## No Leader Election

The mesh has **no leader**. Every mesh pod is equal. Each node acts
autonomously based on local state:

- What it has in its local cache
- What gossipsub tells it peers have (NAR index)
- What the CSI driver requests

This works because NARs are content-addressed and immutable. Two nodes
pushing the same NAR to a third is idempotent -- deduplicated by hash.

The operator has leader election (Lease-based, for CRD reconciliation),
but the mesh layer is fully decentralized.

## libp2p Stack

### Behaviour

```rust
use libp2p::{
    tcp, quic, noise, yamux,
    identify, gossipsub, request_response,
    swarm::NetworkBehaviour,
};

#[derive(NetworkBehaviour)]
struct MeshBehaviour {
    identify: identify::Behaviour,
    gossipsub: gossipsub::Behaviour,
    /// Control plane: HasNar, QueryNarInfo
    control: request_response::cbor::Behaviour<MeshRequest, MeshResponse>,
    /// Data plane: raw streaming for bulk NAR transfers
    transfer: libp2p_stream::Behaviour,
}
```

| Layer | Choice | Why |
|-------|--------|-----|
| Transport | TCP + QUIC | TCP for reliability, QUIC for low-latency (UDP) |
| Encryption | Noise XX | Mutual auth, PFS, no TLS cert management |
| Multiplexing | Yamux | Standard for libp2p Rust |
| Announcements | Gossipsub | Pubsub Have/Evicted, peers learn what's available |
| Content index | Local HashMap | O(1) lookup: "who has NAR X?" |
| Control | Request-Response (CBOR) | HasNar, QueryNarInfo |
| Data transfer | libp2p_stream | Raw AsyncRead/AsyncWrite, zero-copy streaming |
| Identity | Identify | Exchange peer metadata (version, zone, etc.) |

### Port

Single port: `4001` (TCP + QUIC). Same as IPFS convention.

### Gossipsub Config

```rust
// Tuned for K8s clusters. Matches Ethereum beacon chain production
// config (validated at 100k+ validators).
let gossipsub_config = gossipsub::ConfigBuilder::default()
    .heartbeat_interval(Duration::from_millis(700))
    .mesh_n(8)                    // target mesh degree
    .mesh_n_low(6)                // minimum before grafting
    .mesh_n_high(12)              // maximum before pruning
    .history_length(6)            // IHAVE history for reliability
    .history_gossip(3)            // gossip to D_lazy peers
    .max_transmit_size(65536)     // gossip messages are small (~100 bytes)
    .build()
    .unwrap();
```

## Protocol Messages

### Gossipsub (announcements)

Single topic: `niphas/nar/v1`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
enum GossipMessage {
    /// Peer has a new NAR available in its local cache.
    Have { nar_hash: String, store_path: String, nar_size: u64 },
    /// Peer evicted a NAR from local cache (GC).
    Evicted { nar_hash: String },
}
```

When a message arrives:
- `Have`: add peer to NAR index
- `Evicted`: remove peer from NAR index for that hash
- Peer disconnects: remove all entries for that peer

### Control plane (Request-Response CBOR)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MeshRequest {
    /// Check if peer has a NAR.
    HasNar { nar_hash: String },

    /// Query narinfo for a store path.
    QueryNarInfo { store_path_hash: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MeshResponse {
    HasNar { available: bool, nar_size: u64 },
    NarInfo { narinfo: String },
    NotFound,
    Busy,
}
```

### Data plane (libp2p_stream)

NAR transfers use raw substreams via `libp2p_stream::Behaviour`.

Stream protocol ID: `/niphas/nar-transfer/1`

```rust
/// Sent at the start of a transfer substream.
struct TransferHeader {
    nar_hash: [u8; 32],
    offset: u64,      // for parallel range requests (0 = from start)
    length: u64,      // 0 = send everything from offset to end
}

/// Header is 48 bytes, fixed layout, big-endian.
/// After the header, sender streams raw compressed NAR bytes until
/// `length` bytes are sent, then closes the substream write half.
```

The receiver:
1. Opens substream, sends `TransferHeader`
2. Sender streams compressed NAR bytes directly from disk (zero-copy via mmap)
3. Receiver writes to temp file, verifies NarHash after all bytes received
4. If valid: moves to cache, announces via gossipsub `Have`
5. If invalid: discards temp file, logs warning

### Parallel range download (swarming)

For NARs > 64 MB, the receiver can open substreams to **multiple peers**
simultaneously, each requesting a different byte range:

```
NAR = 256 MB, 4 peers available

Peer A: offset=0,    length=64MB
Peer B: offset=64MB, length=64MB
Peer C: offset=128MB,length=64MB
Peer D: offset=192MB,length=64MB

All download in parallel -> ~4x throughput
Receiver assembles chunks, verifies NarHash of the whole file
```

For NARs < 64 MB (the vast majority), single-peer transfer is sufficient.

## NAR Index

Each mesh node maintains a local HashMap tracking which peers have which
NARs. Populated entirely from gossipsub messages.

```rust
use std::collections::{HashMap, HashSet};

struct NarIndex {
    index: HashMap<String, HashSet<PeerId>>,
}

impl NarIndex {
    /// O(1) lookup, zero network latency.
    fn providers(&self, nar_hash: &str) -> Vec<PeerId> {
        self.index.get(nar_hash)
            .map(|peers| peers.iter().copied().collect())
            .unwrap_or_default()
    }

    fn add(&mut self, nar_hash: &str, peer: PeerId) {
        self.index.entry(nar_hash.to_string()).or_default().insert(peer);
    }

    fn remove_peer(&mut self, peer: &PeerId) {
        for peers in self.index.values_mut() {
            peers.remove(peer);
        }
        self.index.retain(|_, peers| !peers.is_empty());
    }
}
```

Memory: only tracks NARs that at least one peer announced. With no full
replication, each peer only announces what it cached for its pods. A node
running 20 microservices with ~3000 unique store paths:
~3000 entries * ~80 bytes = ~240 KB. Negligible.

## Peer Discovery

### Bootstrap: Headless Service DNS

```yaml
apiVersion: v1
kind: Service
metadata:
  name: niphas-mesh
  namespace: niphas-system
spec:
  clusterIP: None
  selector:
    app: niphas-mesh
  ports:
    - port: 4001
      name: libp2p
```

On startup, resolve `niphas-mesh.niphas-system.svc.cluster.local`
to get all mesh pod IPs. Dial each one.

```rust
async fn bootstrap_peers(service_dns: &str) -> Vec<Multiaddr> {
    let addrs = tokio::net::lookup_host(format!("{}:4001", service_dns)).await?;
    addrs.map(|addr| {
        format!("/ip4/{}/tcp/4001", addr.ip()).parse().unwrap()
    }).collect()
}
```

### Dynamic: K8s API Watch

Watch mesh pods for real-time membership changes + node labels.

```rust
async fn watch_mesh_pods(client: kube::Client) {
    let pods: Api<Pod> = Api::namespaced(client, "niphas-system");
    let params = ListParams::default().labels("app=niphas-mesh");
    let mut stream = watcher(pods, params).boxed();

    while let Some(event) = stream.next().await {
        match event? {
            Event::Applied(pod) => {
                let ip = pod.status?.pod_ip?;
                let node = pod.spec?.node_name?;
                let zone = get_node_label(&node, "topology.kubernetes.io/zone");
                peer_registry.add(PeerInfo { ip, node, zone, .. });
            }
            Event::Deleted(pod) => {
                peer_registry.remove(pod.metadata.name);
            }
            _ => {}
        }
    }
}
```

## CSI Interface

The CSI driver communicates with the local mesh pod via Unix socket.

### Socket path

```
/var/run/niphas/mesh.sock
```

Shared via a hostPath volume between the CSI DaemonSet and mesh DaemonSet.

### Protocol (length-prefixed JSON over UDS)

```rust
/// CSI -> Mesh requests
enum CsiRequest {
    /// Fetch a NAR. Mesh tries peers, returns path or error.
    FetchNar {
        store_path: String,
        nar_hash: String,
        nar_size: u64,
    },
    /// Check if a NAR is available on any peer.
    HasNar {
        nar_hash: String,
    },
}

/// Mesh -> CSI responses
enum CsiResponse {
    /// NAR fetched from peer and now available locally.
    Fetched { cache_path: String },
    /// NAR not available from any mesh peer.
    NotFound,
    /// Error during fetch.
    Error { message: String },
}
```

### Fetch flow (CSI perspective)

```
1. CSI checks local cache (/var/lib/niphas/cache/<hash>-<name>/)
   --> if exists: mount directly, done (common case after first run)

2. CSI checks if mesh socket exists (/var/run/niphas/mesh.sock)
   --> if no mesh running: skip to step 5

3. CSI asks mesh via UDS: FetchNar { store_path, nar_hash, nar_size }

4. Mesh queries local NAR index: nar_index.providers(hash)
   --> if found: fetch from best peer (same zone preferred)
   --> verify signature + NarHash
   --> write to local cache
   --> publish gossipsub Have { ... }
   --> respond Fetched { cache_path }
   --> if not found: respond NotFound

5. CSI falls back to binary cache HTTP (direct fetch)
   --> uses CacheClient from niphas-core
   --> verify signature + NarHash
   --> write to local cache

6. If all sources fail:
   --> return gRPC Unavailable to kubelet
   --> kubelet retries with backoff
```

The CSI driver does NOT depend on the mesh. If `mesh.sock` does not exist
(mesh disabled or not yet started), CSI goes directly to binary cache HTTP.

## Cache Management

### Per-node, lazy cache

Each node only caches NARs that its pods actually used. No proactive
replication, no background sync.

```
/var/lib/niphas/cache/
  abc123-hello-2.12.1/         # extracted, used by a local pod
  def456-glibc-2.38/           # dependency of hello, also cached
  .tmp-xyz789/                 # in-progress extraction
```

Cache grows as workloads are deployed. Shared dependencies (glibc,
coreutils, etc.) are fetched once and reused by all subsequent closures.

### Garbage collection (per-node, autonomous)

No cluster coordination needed. Each node manages its own cache with
simple LRU eviction.

```rust
struct GcPolicy {
    /// Start evicting when disk usage exceeds this
    high_watermark_percent: u8,   // default: 85
    /// Stop evicting when disk drops below this
    low_watermark_percent: u8,    // default: 75
}

fn gc_local(cache: &mut Cache, policy: &GcPolicy) -> Result<()> {
    while cache.disk_usage_percent() > policy.low_watermark_percent {
        // Get LRU NAR that is NOT mounted by a local pod
        let candidate = cache.lru_unmounted()?;
        cache.evict(&candidate.nar_hash)?;

        // Announce eviction so peers remove us from their NAR index
        gossipsub.publish("niphas/nar/v1", GossipMessage::Evicted {
            nar_hash: candidate.nar_hash.clone(),
        });
    }
    Ok(())
}
```

**Invariant:** never evict a NAR that is mounted by a local pod (CSI
ref-counting prevents this).

### Cache config (deployer-controlled)

```yaml
# ConfigMap: niphas-config
data:
  config.yaml: |
    cache:
      path: /var/lib/niphas/cache       # overridable for dedicated disk
      highWatermarkPercent: 85
      lowWatermarkPercent: 75
    binaryCaches:
      - url: "https://cache.company.com"
        priority: 1
        publicKey: "cache.company.com-1:..."
      - url: "https://cache.nixos.org"
        priority: 2
        publicKey: "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY="
    mesh:
      bandwidthLimitMbps: 500
      peerFetchTimeout: 5s
```

## Bandwidth Control

### Per-peer rate limiting

```rust
use governor::{Quota, RateLimiter};

struct BandwidthController {
    /// Global outbound rate limit
    global: RateLimiter<...>,
    /// Per-peer outbound rate limit
    per_peer: DashMap<PeerId, RateLimiter<...>>,
}
```

### Priority queue

```rust
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum TransferPriority {
    CsiMount = 0,          // highest: pod waiting for volume
    PeerRequest = 1,       // another node's CSI needs this NAR
}

struct TransferQueue {
    queue: BinaryHeap<PrioritizedTransfer>,
    semaphore: Semaphore,  // limit concurrent transfers (e.g. 8)
}
```

### Connection limits

```rust
let swarm = SwarmBuilder::with_existing_identity(keypair)
    .with_tokio()
    .with_tcp(...)
    .with_quic()
    .with_behaviour(|key| MeshBehaviour::new(key))?
    .with_swarm_config(|cfg| {
        cfg.with_max_negotiating_inbound_streams(128)
           .with_idle_connection_timeout(Duration::from_secs(60))
    })
    .build();
```

Each node does NOT connect to all peers. Gossipsub maintains D=8 mesh
connections. Transfer streams open on-demand and close after transfer.

## Authentication

### Noise XX Handshake

Every connection uses libp2p Noise XX:
- Both peers prove possession of their Ed25519 identity key
- Ephemeral Curve25519 keys per session (PFS)
- Post-handshake encryption: ChaCha20-Poly1305

### Closed Mesh (PeerId Allowlist)

Only known cluster members can connect. The peer registry (from K8s API
watch) provides the allowlist.

```rust
impl MeshBehaviour {
    fn handle_new_connection(&mut self, peer_id: &PeerId) -> Result<(), ConnectionDenied> {
        if !self.peer_registry.contains(peer_id) {
            tracing::warn!(%peer_id, "rejected connection from unknown peer");
            return Err(ConnectionDenied::new("peer not in cluster"));
        }
        Ok(())
    }
}
```

### Key Generation

Each mesh pod generates an Ed25519 keypair on first boot and stores it
in a K8s Secret (one per node):

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: niphas-mesh-identity-<node-name>
  namespace: niphas-system
type: Opaque
data:
  private-key: <base64-encoded-ed25519-private-key>
```

The PeerId (derived from the public key) is published as a pod annotation:

```yaml
metadata:
  annotations:
    niphas.io/peer-id: "12D3KooW..."
```

### Key Rotation

1. Delete the Secret
2. Restart the mesh pod
3. Pod generates new keypair, publishes new PeerId
4. Other pods see updated annotation via K8s API watch
5. Old PeerId expires from allowlist

## Peer Metadata

```rust
struct PeerMetadata {
    zone: String,
    hostname: String,
    available_disk: u64,
    load_percent: u8,
    cached_paths: u32,
    version: String,
}
```

Exchanged via the Identify protocol (periodic, every 5 minutes).
Used for peer selection (same-zone preference, least-loaded fallback).

## Observability

### Metrics (Prometheus)

```
niphas_mesh_peers_connected            gauge    # connected mesh peers
niphas_mesh_nar_fetch_total            counter  # fetches by source (cache/peer/http)
niphas_mesh_nar_fetch_bytes_total      counter  # bytes fetched
niphas_mesh_nar_fetch_duration_seconds histogram # fetch latency
niphas_mesh_nar_serve_total            counter  # NARs served to other peers
niphas_mesh_gossip_messages_total      counter  # gossipsub messages (have/evicted)
niphas_mesh_bandwidth_bytes_total      counter  # total bandwidth (in/out)
niphas_mesh_cache_size_bytes           gauge    # local cache disk usage
niphas_mesh_cache_paths                gauge    # cached store paths
niphas_mesh_gc_evictions_total         counter  # NARs evicted by GC
```

### Structured logs

```
level=info msg="NAR fetched from peer" store_path=/nix/store/abc123-hello nar_hash=sha256:... source_peer=12D3KooW... zone=us-east-1a bytes=12345 duration_ms=42
level=info msg="NAR served to peer" nar_hash=sha256:... target_peer=12D3KooW... bytes=12345 duration_ms=38
level=info msg="cache miss, falling back to HTTP" store_path=/nix/store/abc123-hello cache=https://cache.company.com
level=warn msg="NAR signature verification failed" nar_hash=sha256:... source=mesh peer=12D3KooW...
level=info msg="GC eviction" nar_hash=sha256:... reason=disk_pressure
```

## Failure Scenarios

| Failure | Behavior |
|---------|----------|
| Mesh pod dies | CSI still works: falls back to binary cache HTTP. Existing mounts survive. Local cache persists (hostPath) |
| Mesh disabled (`mesh.enabled: false`) | CSI fetches directly from binary cache HTTP. No P2P, no gossipsub, no mesh pods |
| No peer has the NAR | Mesh responds NotFound, CSI falls back to binary cache HTTP |
| Peer disconnects mid-transfer | Receiver discards partial data, tries another peer or falls back to HTTP |
| Binary cache down + no peer has NAR | Pod stuck in ContainerCreating, kubelet retries with backoff |
| Node dies | Pods rescheduled on another labeled node. CSI fetches closure from local cache (if previously used) or mesh peers or binary cache |
| Corrupted NAR from peer | Hash verification fails, discarded, try next peer or HTTP |
| Disk full | GC evicts LRU unmounted NARs until below low watermark |
| New labeled node joins | CSI + mesh pods start. Cache is empty. First workload fetches from peers/HTTP, caches locally |
| Label removed from node | CSI + mesh pods evicted. Cache persists on hostPath. Re-labeling restores warm cache |
