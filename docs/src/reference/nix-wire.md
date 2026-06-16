# Nix Wire Formats & Binary Cache Protocol

How niphas-core implements NAR parsing, `.narinfo` handling, Nix hashing, signature verification, and binary cache interaction.

## Module Layout

All wire format code lives in `niphas-core`:

| Module | Purpose |
|--------|---------|
| `nar.rs` | NAR streaming parser and extractor |
| `narinfo.rs` | `.narinfo` text parser |
| `hash.rs` | Nix-base32 encoding, SHA-256 |
| `signature.rs` | Ed25519 signature verification |
| `store_path.rs` | Store path validation |
| `cache_client.rs` | Binary cache HTTP client |
| `closure.rs` | Parallel closure resolution |

## NAR Format

NAR (Nix ARchive) is a deterministic serialization format for file system objects.

**Wire primitives:**
- 64-bit unsigned little-endian integers
- Length-prefixed strings padded to 8-byte alignment

**Node types:** `regular` (files), `symlink`, `directory` (with sorted entries)

**Constraints:**
- Magic: `nix-archive-1`
- Directory entries must be strictly ascending (no duplicates)
- No `/` or `\0` in entry names
- Depth limit: 256
- Zero padding, no trailing data

**Extractor safety (CVE-2024-45593):**
- Atomic extraction to `.tmp-*` then rename
- Symlink validation via `O_NOFOLLOW` and `*at()` syscalls
- No symlink following during extraction

## `.narinfo` Format

Plain text `field=value` pairs:

```
StorePath: /nix/store/abc...-hello-2.12
URL: nar/abc....nar.zst
Compression: zstd
FileHash: sha256:abc...
FileSize: 12345
NarHash: sha256:def...
NarSize: 67890
References: abc...-glibc-2.38 def...-hello-2.12
Sig: cache.nixos.org-1:base64signature...
```

## Nix Hashing

SHA-256 with a custom base32 encoding:

- **Nix-base32 alphabet:** `0123456789abcdfghijklmnpqrsvwxyz` (omits `e`, `o`, `t`, `u`)
- Encoding is reversed (LSB first)
- Store path hash: 32-char Nix-base32 in `/nix/store/<hash>-<name>`

## Ed25519 Signature Verification

Fingerprint format:

```
1;<store-path>;<nar-hash>;<nar-size>;<references>
```

At least one signature must validate against trusted public keys. Unverified NARs are rejected.

## Verification Chain

Every NAR goes through a complete verification pipeline:

1. Fetch `.narinfo` → verify Ed25519 signature
2. Download compressed NAR → verify `FileHash`
3. Decompress → verify `NarHash` (inline streaming)
4. Extract → atomic rename

**Unverified NAR never reaches a pod.**

## Binary Cache HTTP Client

**Endpoints:**
- `GET /nix-cache-info` — cache metadata
- `GET /<hash>.narinfo` — store path info
- `GET /nar/<file-hash>.nar.zst` — compressed NAR data

The client supports priority-ordered cache lists and streams large NARs without loading them entirely into memory.

## Closure Resolution

Pure HTTP, no Nix needed. Recursive BFS of `.narinfo` `References` fields with parallel resolution (configurable concurrency, default 32).

## Decompression

Supported formats: `zstd` (most common), `xz`, `bzip2`, `br` (brotli), `none`. Uses `async-compression` for streaming decompression.
