# Niphas -- Nix Wire Formats & Binary Cache Protocol

Niphas implements all Nix format parsing and verification in-house.
Zero dependency on nix-compat, libnar, or any external Nix crate.
Everything lives in `niphas-core` as shared modules consumed by CSI, mesh, and eval.

## Module layout (niphas-core)

```
niphas-core/src/
  nix/
    mod.rs
    nar.rs          # NAR archive parser + extractor
    narinfo.rs      # .narinfo text parser
    hash.rs         # Nix hashing (SHA-256, Nix-base32)
    signature.rs    # Ed25519 signature verification
    store_path.rs   # Store path parsing + validation
    cache_client.rs # Binary cache HTTP client
    closure.rs      # Recursive closure resolution
```

---

## NAR format

NAR (Nix Archive) is a deterministic binary archive. Same filesystem tree
always produces the exact same bytes. No timestamps, no ownership, no
extended attributes.

### Wire primitives

**Integer:** 64-bit unsigned, little-endian.

```
encode_u64(n) = n as [u8; 8] in LE
```

**String/bytes:** Length-prefixed, padded to 8-byte alignment.

```
encode_bytes(b) =
  encode_u64(b.len())
  + b
  + zero_pad(b.len())    # 0..7 null bytes to reach next 8-byte boundary

zero_pad(len) = [0u8; (8 - (len % 8)) % 8]
```

### Grammar

```
nar          = str("nix-archive-1") node

node         = str("(") node_body str(")")

node_body    = str("type") str("regular")   regular_body
             | str("type") str("symlink")   symlink_body
             | str("type") str("directory") directory_body

regular_body = [ str("executable") str("") ]
               str("contents") bytes(file_data)

symlink_body = str("target") str(target_path)

directory_body = { entry }    # zero or more, sorted by name ASC

entry        = str("entry") str("(")
               str("name") str(entry_name)
               str("node") node
               str(")")
```

Every token (`"nix-archive-1"`, `"("`, `"type"`, `"regular"`, etc.) is
encoded via `encode_bytes()`. File contents use `bytes()` (same encoding).

### Constraints the parser must enforce

| Rule | Reject if |
|------|-----------|
| Magic | First token != `"nix-archive-1"` |
| Entry names | Contains `/`, `\0`, or equals `.` or `..` or is empty |
| Entry order | Current name <= previous name (must be strictly ascending) |
| Duplicate names | Same name appears twice in a directory |
| Nesting depth | Exceeds configurable limit (default: 256) |
| File size | `contents` length > configurable max (from NarSize in narinfo) |
| Padding | Padding bytes are not all zero |
| Trailing data | Bytes remain after the root node closes |

### Parser design (streaming, zero-alloc where possible)

```rust
/// Token-level reader over an AsyncRead stream.
struct NarReader<R> {
    inner: R,
    buf: [u8; 8],         // reused for every u64/padding read
    depth: u16,           // current nesting depth
    max_depth: u16,       // configurable limit (default 256)
    bytes_read: u64,      // for NarHash computation
    hasher: Sha256,       // computes hash inline during parse
}

impl<R: AsyncRead + Unpin> NarReader<R> {
    /// Read a length-prefixed byte string.
    async fn read_bytes(&mut self) -> Result<Vec<u8>>;

    /// Read a length-prefixed string, validate UTF-8.
    async fn read_str(&mut self) -> Result<String>;

    /// Read exactly the expected string token, error otherwise.
    async fn expect_str(&mut self, expected: &str) -> Result<()>;

    /// Read a u64 (8 bytes LE).
    async fn read_u64(&mut self) -> Result<u64>;

    /// Skip padding bytes (0..7 after a string), validate all zero.
    async fn skip_padding(&mut self, len: u64) -> Result<()>;
}
```

The parser never loads the entire NAR into memory. It streams entries
and writes each file/symlink/directory to disk as it encounters them.

### Extractor design

```rust
/// Extracts a NAR stream into a target directory.
///
/// Safety invariants:
/// - Target dir must be inside /var/lib/niphas/cache/
/// - Extraction happens into a .tmp-<uuid> dir, atomically renamed on success
/// - Symlinks are created but NEVER followed during extraction
/// - All paths resolved relative to extraction root, never escaping
struct NarExtractor {
    root: PathBuf,         // e.g. /var/lib/niphas/cache/.tmp-<uuid>/
    max_nar_size: u64,     // from narinfo NarSize
}

impl NarExtractor {
    /// Extract NAR from reader into root directory.
    /// Returns the SHA-256 hash of the NAR byte stream.
    async fn extract<R: AsyncRead + Unpin>(
        &self,
        reader: R,
    ) -> Result<NarHash>;
}
```

#### Symlink safety (CVE-2024-45593 mitigation)

The Nix reference implementation had a critical vuln (CVSS 9.0) where
a NAR containing a symlink followed by a directory of the same name
could write files outside the extraction root.

Mitigation:

```rust
fn extract_entry(&self, parent_fd: RawFd, name: &str, node: Node) -> Result<()> {
    // Validate entry name
    if name.contains('/') || name.contains('\0') || name == "." || name == ".." || name.is_empty() {
        return Err(NarError::InvalidEntryName(name.into()));
    }

    match node {
        Node::Regular { executable, contents } => {
            // O_NOFOLLOW: if `name` is an existing symlink, this fails
            // O_EXCL: fail if entry already exists (catches duplicate names)
            let fd = openat(parent_fd, name, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, mode)?;
            write_all(fd, contents)?;
            if executable {
                fchmod(fd, 0o555)?;
            } else {
                fchmod(fd, 0o444)?;
            }
        }
        Node::Symlink { target } => {
            // Create symlink. Do NOT validate target -- Nix allows
            // arbitrary symlink targets (e.g. /nix/store/... or relative).
            // Safety comes from never following symlinks, not from restricting targets.
            symlinkat(target, parent_fd, name)?;
        }
        Node::Directory { entries } => {
            // O_NOFOLLOW on mkdirat: if `name` is a symlink, this fails
            mkdirat(parent_fd, name, 0o755)?;
            let dir_fd = openat(parent_fd, name, O_RDONLY | O_DIRECTORY | O_NOFOLLOW, 0)?;
            for entry in entries {
                self.extract_entry(dir_fd, &entry.name, entry.node)?;
            }
        }
    }
    Ok(())
}
```

Key: every filesystem operation uses `*at()` syscalls (`openat`, `mkdirat`,
`symlinkat`) relative to a parent fd. Combined with `O_NOFOLLOW`, this
makes symlink-based escapes impossible regardless of NAR contents.

#### Atomic extraction

```
1. mkdtemp("/var/lib/niphas/cache/.tmp-XXXXXX")
2. extract NAR into temp dir
3. verify computed NarHash == expected NarHash
4. rename(temp_dir, "/var/lib/niphas/cache/<hash>-<name>/")
5. if rename fails (exists): another thread won the race, delete temp
```

If the process crashes mid-extraction, `.tmp-*` dirs are cleaned on
driver startup (they are always incomplete/unverified).

---

## .narinfo format

Plain text, one field per line, colon-separated.

```
StorePath: /nix/store/abc123-hello-2.12.1
URL: nar/1w1fff338fvdw53sqgamddn1b2xgds473pv6y13gizdbqjv4i5p3.nar.zst
Compression: zstd
FileHash: sha256:1w1fff...
FileSize: 12345
NarHash: sha256:1impfh...
NarSize: 45678
References: abc123-hello-2.12.1 def456-glibc-2.38 ghi789-zlib-1.3.1
Deriver: xyz-hello-2.12.1.drv
Sig: cache.nixos.org-1:GrGV/Ls10Tzo...base64...
Sig: company-cache-1:aBcD...base64...
```

### Fields

| Field | Required | Type | Description |
|-------|----------|------|-------------|
| StorePath | yes | store path | Full `/nix/store/<hash>-<name>` |
| URL | yes | relative URL | Path to compressed NAR file |
| Compression | yes | enum | `none`, `xz`, `bzip2`, `zstd`, `br`, `lzip`, `lz4` |
| FileHash | yes | `<algo>:<nix-base32>` | Hash of compressed file |
| FileSize | yes | u64 | Size of compressed file in bytes |
| NarHash | yes | `<algo>:<nix-base32>` | Hash of uncompressed NAR stream |
| NarSize | yes | u64 | Size of uncompressed NAR in bytes |
| References | no | space-separated | Runtime dependencies (basename only, no `/nix/store/` prefix) |
| Deriver | no | string | The `.drv` that produced this path |
| Sig | no (repeatable) | `<keyname>:<base64>` | Ed25519 signatures |
| CA | no | string | Content address (for CA paths) |

### Parser

```rust
pub struct NarInfo {
    pub store_path: StorePath,
    pub url: String,
    pub compression: Compression,
    pub file_hash: NixHash,
    pub file_size: u64,
    pub nar_hash: NixHash,
    pub nar_size: u64,
    pub references: Vec<StorePathRef>,  // basenames only
    pub deriver: Option<String>,
    pub signatures: Vec<NarSignature>,
    pub ca: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub enum Compression {
    None,
    Xz,
    Bzip2,
    Zstd,
    Br,
    Lzip,
    Lz4,
}

pub struct NarSignature {
    pub key_name: String,      // e.g. "cache.nixos.org-1"
    pub signature: [u8; 64],   // Ed25519 signature bytes
}

impl NarInfo {
    /// Parse from narinfo text content.
    pub fn parse(input: &str) -> Result<Self, NarInfoError> {
        let mut store_path = None;
        let mut url = None;
        // ... field-by-field parsing

        for line in input.lines() {
            let (key, value) = line.split_once(": ")
                .ok_or(NarInfoError::MalformedLine)?;
            match key {
                "StorePath" => store_path = Some(StorePath::parse(value)?),
                "URL" => url = Some(value.to_owned()),
                "Compression" => compression = Some(Compression::parse(value)?),
                "NarHash" => nar_hash = Some(NixHash::parse(value)?),
                "NarSize" => nar_size = Some(value.parse::<u64>()?),
                "FileHash" => file_hash = Some(NixHash::parse(value)?),
                "FileSize" => file_size = Some(value.parse::<u64>()?),
                "References" => {
                    references = value.split_whitespace()
                        .map(StorePathRef::parse)
                        .collect::<Result<Vec<_>>>()?;
                }
                "Sig" => {
                    signatures.push(NarSignature::parse(value)?);
                }
                "Deriver" => deriver = Some(value.to_owned()),
                "CA" => ca = Some(value.to_owned()),
                _ => {} // ignore unknown fields (forward compat)
            }
        }

        Ok(NarInfo { /* ... */ })
    }
}
```

---

## Nix hashing

Nix uses SHA-256 with a custom base32 encoding.

### Nix-base32

Alphabet: `0123456789abcdfghijklmnpqrsvwxyz` (32 chars, omits `e o t u`).

Encoding is **reversed** compared to standard base32: the least significant
bits come first.

```rust
const NIX_BASE32_CHARS: &[u8; 32] = b"0123456789abcdfghijklmnpqrsvwxyz";

/// Encode bytes to Nix base32.
pub fn to_nix_base32(input: &[u8]) -> String {
    let len = (input.len() * 8 + 4) / 5; // ceil(bits / 5)
    let mut out = String::with_capacity(len);

    for n in (0..len).rev() {
        let b = n * 5;
        let byte_idx = b / 8;
        let bit_idx = b % 8;

        let mut c = (input[byte_idx] >> bit_idx) & 0x1f;
        if bit_idx > 3 && byte_idx + 1 < input.len() {
            c |= input[byte_idx + 1] << (8 - bit_idx);
            c &= 0x1f;
        }
        out.push(NIX_BASE32_CHARS[c as usize] as char);
    }

    out
}

/// Decode Nix base32 to bytes.
pub fn from_nix_base32(input: &str) -> Result<Vec<u8>, NixHashError> {
    // Reverse of the above
}
```

### NixHash type

```rust
pub struct NixHash {
    pub algo: HashAlgo,
    pub digest: Vec<u8>,   // raw bytes
}

#[derive(Debug, Clone, Copy)]
pub enum HashAlgo {
    Sha256,
    Sha512,
    Sha1,     // legacy, some old paths use this
    Md5,      // legacy
}

impl NixHash {
    /// Parse "sha256:1impfh..." (Nix-base32 encoded).
    pub fn parse(s: &str) -> Result<Self, NixHashError> {
        let (algo_str, hash_str) = s.split_once(':')
            .ok_or(NixHashError::MissingAlgo)?;
        let algo = match algo_str {
            "sha256" => HashAlgo::Sha256,
            "sha512" => HashAlgo::Sha512,
            "sha1" => HashAlgo::Sha1,
            "md5" => HashAlgo::Md5,
            _ => return Err(NixHashError::UnknownAlgo(algo_str.into())),
        };
        let digest = from_nix_base32(hash_str)?;
        Ok(NixHash { algo, digest })
    }

    /// Verify that data matches this hash.
    pub fn verify(&self, data: &[u8]) -> Result<(), NixHashError> {
        use sha2::{Sha256, Digest};
        match self.algo {
            HashAlgo::Sha256 => {
                let computed = Sha256::digest(data);
                if computed.as_slice() != self.digest {
                    return Err(NixHashError::Mismatch);
                }
            }
            // other algos...
        }
        Ok(())
    }
}
```

### Store path hash

The 32-char hash in `/nix/store/<hash>-<name>` is **not** a SHA-256 of
the contents. It is derived from the derivation inputs via a truncated
SHA-256, then Nix-base32 encoded. For Niphas, we don't need to compute
this -- we receive it from `nix eval`. We only need to parse and validate it.

```rust
pub struct StorePath {
    pub hash: [u8; 20],   // 20 bytes = 32 Nix-base32 chars
    pub name: String,      // e.g. "hello-2.12.1"
}

pub struct StorePathRef(String);  // basename only, e.g. "abc123-hello-2.12.1"

impl StorePath {
    /// Parse "/nix/store/abc123...-hello-2.12.1"
    pub fn parse(s: &str) -> Result<Self, StorePathError> {
        let rest = s.strip_prefix("/nix/store/")
            .ok_or(StorePathError::InvalidPrefix)?;
        if rest.len() < 34 { // 32 hash + "-" + at least 1 char name
            return Err(StorePathError::TooShort);
        }
        let hash_str = &rest[..32];
        let name = &rest[33..]; // skip the "-"
        let hash_bytes = from_nix_base32(hash_str)?;
        Ok(StorePath {
            hash: hash_bytes.try_into().map_err(|_| StorePathError::InvalidHash)?,
            name: name.to_owned(),
        })
    }

    /// The 32-char Nix-base32 hash prefix, used for .narinfo lookup.
    pub fn hash_str(&self) -> String {
        to_nix_base32(&self.hash)
    }
}
```

---

## Ed25519 signature verification

### Fingerprint format

The signature covers a "fingerprint" string:

```
1;<store-path>;<nar-hash>;<nar-size>;<references>
```

Where:
- `1` is the fingerprint version
- `<store-path>` is the full path (e.g. `/nix/store/abc123-hello-2.12.1`)
- `<nar-hash>` is `sha256:<nix-base32>` (the NarHash field from narinfo)
- `<nar-size>` is decimal NarSize
- `<references>` is comma-separated sorted store paths of References

Example:
```
1;/nix/store/abc123-hello-2.12.1;sha256:1impfh...;45678;/nix/store/def456-glibc-2.38,/nix/store/ghi789-zlib-1.3.1
```

### Verification

Dependencies: `ed25519-dalek` (already audited, widely used).

```rust
use ed25519_dalek::{Signature, VerifyingKey, Verifier};

pub struct TrustedKey {
    pub name: String,              // e.g. "cache.nixos.org-1"
    pub pubkey: VerifyingKey,      // 32-byte Ed25519 public key
}

impl TrustedKey {
    /// Parse "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY="
    pub fn parse(s: &str) -> Result<Self, SignatureError> {
        let (name, key_b64) = s.split_once(':')
            .ok_or(SignatureError::MalformedKey)?;
        let key_bytes = base64_decode(key_b64)?;
        let pubkey = VerifyingKey::from_bytes(
            &key_bytes.try_into().map_err(|_| SignatureError::InvalidKeyLength)?
        )?;
        Ok(TrustedKey { name: name.to_owned(), pubkey })
    }
}

/// Compute the fingerprint string that signatures cover.
fn compute_fingerprint(narinfo: &NarInfo) -> String {
    let refs: Vec<String> = narinfo.references.iter()
        .map(|r| format!("/nix/store/{}", r.0))
        .collect();
    let mut refs_sorted = refs.clone();
    refs_sorted.sort();

    format!(
        "1;{};{};{};{}",
        narinfo.store_path.to_string(),
        narinfo.nar_hash.to_nix_string(),  // "sha256:<nix-base32>"
        narinfo.nar_size,
        refs_sorted.join(",")
    )
}

/// Verify that at least one signature matches a trusted key.
pub fn verify_narinfo(
    narinfo: &NarInfo,
    trusted_keys: &[TrustedKey],
) -> Result<(), SignatureError> {
    let fingerprint = compute_fingerprint(narinfo);

    for sig in &narinfo.signatures {
        for key in trusted_keys {
            if sig.key_name == key.name {
                let signature = Signature::from_bytes(&sig.signature);
                if key.pubkey.verify(fingerprint.as_bytes(), &signature).is_ok() {
                    return Ok(());
                }
            }
        }
    }

    Err(SignatureError::NoTrustedSignature {
        store_path: narinfo.store_path.to_string(),
        signatures_present: narinfo.signatures.iter()
            .map(|s| s.key_name.clone())
            .collect(),
    })
}
```

### Invariant

**An unverified NAR never reaches a pod.** The verification chain:

```
1. Fetch .narinfo
2. Verify signature (fingerprint covers NarHash)
3. Fetch compressed NAR
4. Verify FileHash (hash of compressed bytes)
5. Decompress
6. Verify NarHash (hash of decompressed NAR stream) -- computed inline during parse
7. Extract to temp dir
8. Atomic rename to cache dir
```

If any step fails, the NAR is discarded. No partial state persists.

---

## Binary cache HTTP client

### Protocol

```
GET /nix-cache-info                          --> cache metadata
GET /<store-path-hash>.narinfo               --> store path metadata
GET /nar/<file-hash>.nar.zstd                --> compressed NAR archive
```

The `<store-path-hash>` is the 32-char Nix-base32 prefix from the
store path. E.g. for `/nix/store/abc123...-hello`, the request is
`GET /abc123....narinfo`.

### Client design

```rust
pub struct CacheClient {
    http: reqwest::Client,
    caches: Vec<CacheConfig>,      // ordered by priority
    trusted_keys: Vec<TrustedKey>,
}

pub struct CacheConfig {
    pub url: String,               // e.g. "https://cache.nixos.org"
    pub priority: u32,
    pub public_keys: Vec<String>,  // key names trusted for this cache
}

impl CacheClient {
    /// Fetch narinfo for a store path. Tries caches in priority order.
    pub async fn fetch_narinfo(
        &self,
        store_path: &StorePath,
    ) -> Result<(NarInfo, String), CacheError> {
        let hash = store_path.hash_str();

        for cache in &self.caches {
            let url = format!("{}/{}.narinfo", cache.url, hash);
            match self.http.get(&url).send().await {
                Ok(resp) if resp.status() == 200 => {
                    let text = resp.text().await?;
                    let narinfo = NarInfo::parse(&text)?;
                    verify_narinfo(&narinfo, &self.trusted_keys)?;
                    return Ok((narinfo, cache.url.clone()));
                }
                Ok(resp) if resp.status() == 404 => continue,
                Ok(resp) => {
                    tracing::warn!(cache = %cache.url, status = %resp.status(), "unexpected status");
                    continue;
                }
                Err(e) => {
                    tracing::warn!(cache = %cache.url, error = %e, "cache unreachable");
                    continue;
                }
            }
        }

        Err(CacheError::NotFound(store_path.to_string()))
    }

    /// Download and verify a NAR. Returns path to extracted dir.
    pub async fn fetch_nar(
        &self,
        narinfo: &NarInfo,
        cache_url: &str,
        cache_dir: &Path,
    ) -> Result<PathBuf, CacheError> {
        let nar_url = format!("{}/{}", cache_url, narinfo.url);

        // Stream download
        let resp = self.http.get(&nar_url).send().await?;
        let compressed_bytes = resp.bytes().await?;

        // Verify FileHash (compressed)
        narinfo.file_hash.verify(&compressed_bytes)?;

        // Decompress
        let decompressed = decompress(&compressed_bytes, narinfo.compression)?;

        // Extract NAR (NarHash verified inline during extraction)
        let tmp_dir = cache_dir.join(format!(".tmp-{}", uuid()));
        std::fs::create_dir_all(&tmp_dir)?;

        let extractor = NarExtractor::new(&tmp_dir, narinfo.nar_size);
        let computed_hash = extractor.extract(&mut &decompressed[..]).await?;

        // Verify NarHash
        if computed_hash != narinfo.nar_hash {
            std::fs::remove_dir_all(&tmp_dir)?;
            return Err(CacheError::NarHashMismatch);
        }

        // Atomic rename
        let final_dir = cache_dir.join(narinfo.store_path.basename());
        match std::fs::rename(&tmp_dir, &final_dir) {
            Ok(()) => Ok(final_dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Race: another thread/process extracted the same path
                std::fs::remove_dir_all(&tmp_dir)?;
                Ok(final_dir)
            }
            Err(e) => Err(e.into()),
        }
    }
}
```

### Streaming for large NARs

For NARs larger than a configurable threshold (e.g. 64 MB), avoid
loading the entire compressed body into memory:

```rust
pub async fn fetch_nar_streaming(
    &self,
    narinfo: &NarInfo,
    cache_url: &str,
    cache_dir: &Path,
) -> Result<PathBuf, CacheError> {
    let nar_url = format!("{}/{}", cache_url, narinfo.url);
    let resp = self.http.get(&nar_url).send().await?;
    let byte_stream = resp.bytes_stream();

    // Pipeline: download --> hash(compressed) --> decompress --> hash(nar) --> extract
    //
    // FileHash is verified by hashing the compressed stream as it passes through.
    // NarHash is verified by hashing the decompressed stream during extraction.
    // If either hash fails, extraction is aborted and temp dir removed.

    let compressed_hasher = HashingReader::new(byte_stream, narinfo.file_hash.algo);
    let decompressor = ZstdDecoder::new(compressed_hasher); // or xz, etc.
    let nar_reader = NarReader::new(decompressor);

    let tmp_dir = cache_dir.join(format!(".tmp-{}", uuid()));
    std::fs::create_dir_all(&tmp_dir)?;

    let extractor = NarExtractor::new(&tmp_dir, narinfo.nar_size);
    let (computed_nar_hash, computed_file_hash) = extractor
        .extract_streaming(nar_reader)
        .await?;

    // Verify both hashes
    if computed_file_hash != narinfo.file_hash {
        std::fs::remove_dir_all(&tmp_dir)?;
        return Err(CacheError::FileHashMismatch);
    }
    if computed_nar_hash != narinfo.nar_hash {
        std::fs::remove_dir_all(&tmp_dir)?;
        return Err(CacheError::NarHashMismatch);
    }

    // Atomic rename
    let final_dir = cache_dir.join(narinfo.store_path.basename());
    std::fs::rename(&tmp_dir, &final_dir)?;
    Ok(final_dir)
}
```

---

## Closure resolution (pure HTTP, no Nix)

The full closure (all transitive runtime dependencies) can be resolved
by recursively fetching `.narinfo` files from the binary cache.

Each `.narinfo` has a `References` field listing immediate dependencies
(basenames only). Walk this graph to get the complete closure.

```rust
use std::collections::HashSet;

impl CacheClient {
    /// Resolve the full closure of a store path by walking .narinfo References.
    /// Returns all store paths in the closure, including the root.
    pub async fn resolve_closure(
        &self,
        root: &StorePath,
    ) -> Result<Vec<NarInfo>, CacheError> {
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: Vec<StorePath> = vec![root.clone()];
        let mut closure: Vec<NarInfo> = Vec::new();

        while let Some(path) = queue.pop() {
            let basename = path.basename();
            if visited.contains(&basename) {
                continue;
            }
            visited.insert(basename.clone());

            let (narinfo, _cache_url) = self.fetch_narinfo(&path).await?;

            // Enqueue unvisited references
            for ref_basename in &narinfo.references {
                if !visited.contains(&ref_basename.0) {
                    let ref_path = StorePath::from_basename(&ref_basename.0)?;
                    queue.push(ref_path);
                }
            }

            closure.push(narinfo);
        }

        Ok(closure)
    }

    /// Parallel closure resolution: fetch multiple narinfos concurrently.
    pub async fn resolve_closure_parallel(
        &self,
        root: &StorePath,
        concurrency: usize,  // e.g. 16
    ) -> Result<Vec<NarInfo>, CacheError> {
        // BFS with bounded concurrency using tokio::sync::Semaphore
        // or futures::stream::buffer_unordered
    }
}
```

### When closure resolution happens

```
1. User creates NiphasWorkload CR
2. niphas-eval evaluates flake via Nix C API (in-process FFI) --> gets outPath
3. niphas-eval calls resolve_closure(outPath) via HTTP to binary cache
4. niphas-eval writes closure_paths to CRD status
5. operator reads closure_paths from status, passes to CSI via volumeAttributes
6. CSI driver fetches individual NARs (already knows the full list)
```

Both steps happen inside the niphas-eval process. Step 2 uses the Nix C API
via FFI (no process spawn, no Job). Step 3 is pure HTTP -- no Nix needed.

---

## Decompression

Support the compression formats Nix binary caches use:

| Format | Crate | Notes |
|--------|-------|-------|
| zstd | `async-compression` (already in workspace) | Most common for modern caches |
| xz | `async-compression` + `xz2` feature | cache.nixos.org uses this |
| bzip2 | `async-compression` + `bzip2` feature | Legacy |
| br (brotli) | `async-compression` + `brotli` feature | Rare |
| none | passthrough | Uncompressed |

```rust
fn decompress(data: &[u8], compression: Compression) -> Result<Vec<u8>> {
    match compression {
        Compression::None => Ok(data.to_vec()),
        Compression::Zstd => {
            let mut decoder = zstd::Decoder::new(data)?;
            let mut out = Vec::new();
            decoder.read_to_end(&mut out)?;
            Ok(out)
        }
        Compression::Xz => {
            let mut decoder = xz2::read::XzDecoder::new(data);
            let mut out = Vec::new();
            decoder.read_to_end(&mut out)?;
            Ok(out)
        }
        // ...
    }
}
```

For streaming, use `async-compression::tokio::bufread::*Decoder`.

---

## Dependency summary

All new deps for the Nix wire format implementation:

| Crate | Purpose | Already in workspace? |
|-------|---------|-----------------------|
| `sha2` | SHA-256 for NarHash/FileHash | No, add |
| `ed25519-dalek` | Ed25519 signature verification | No, add |
| `base64` | Decode signature bytes and public keys | No, add |
| `reqwest` | HTTP client for binary cache | No, add |
| `uuid` | Temp dir naming | No, add (or use random bytes) |
| `async-compression` | zstd/xz/bzip2 decompression | Yes |
| `tokio` | Async runtime | Yes |
| `serde` | NarInfo struct serialization | Yes |
| `tracing` | Logging | Yes |
| `thiserror` | Error types | Yes |
