use niphas_core::config::NiphasConfig;
use niphas_core::error::NiphasError;
use niphas_core::nix::cache_client::{BinaryCacheClient, CacheClient};
use niphas_core::nix::hash::sha256_hash;
use niphas_core::nix::nar::NarReader;
use niphas_core::nix::narinfo::NarInfo;
use niphas_core::nix::store_path::StorePath;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;
use tracing::{debug, info};

/// Local NAR cache on each node.
///
/// Manages fetched and extracted Nix store paths under a configurable
/// cache directory (default: `/var/lib/niphas/cache`).
///
/// Thread safety: per-path mutexes prevent duplicate concurrent downloads.
///
/// Generic over `C` so tests can inject a fake binary cache client.
pub struct NarCache<C: BinaryCacheClient = CacheClient> {
    cache_dir: PathBuf,
    cache_client: C,
    /// Per-path locks to prevent duplicate downloads.
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl NarCache<CacheClient> {
    pub fn new(config: &NiphasConfig) -> Result<Self, NiphasError> {
        let cache_dir = PathBuf::from(&config.cache.path);
        std::fs::create_dir_all(&cache_dir)?;

        let cache_client = CacheClient::new(config.binary_caches.clone())?;

        Ok(NarCache {
            cache_dir,
            cache_client,
            locks: Mutex::new(HashMap::new()),
        })
    }
}

impl<C: BinaryCacheClient> NarCache<C> {
    #[cfg(test)]
    pub fn with_client(cache_dir: PathBuf, cache_client: C) -> Self {
        NarCache {
            cache_dir,
            cache_client,
            locks: Mutex::new(HashMap::new()),
        }
    }

    /// Return the filesystem path for a given store path basename.
    pub fn path_for(&self, store_path: &str) -> String {
        // Strip /nix/store/ prefix if present.
        let basename = store_path
            .strip_prefix("/nix/store/")
            .unwrap_or(store_path);
        self.cache_dir.join(basename).to_string_lossy().to_string()
    }

    /// Check if a store path is already cached locally.
    fn is_cached(&self, basename: &str) -> bool {
        self.cache_dir.join(basename).exists()
    }

    /// Ensure all closure paths are present in the local cache.
    /// Fetches missing paths from binary caches.
    pub async fn ensure_cached(
        &self,
        closure_paths: &[&str],
        _binary_cache_url: Option<&str>,
    ) -> Result<(), NiphasError> {
        for &path in closure_paths {
            let basename = path
                .strip_prefix("/nix/store/")
                .unwrap_or(path);

            if self.is_cached(basename) {
                debug!(basename, "already cached");
                continue;
            }

            // Get or create per-path lock to prevent duplicate downloads.
            let lock = {
                let mut locks = self.locks.lock().await;
                locks
                    .entry(basename.to_string())
                    .or_insert_with(|| Arc::new(Mutex::new(())))
                    .clone()
            };

            let _guard = lock.lock().await;

            // Double-check after acquiring lock.
            if self.cache_dir.join(basename).exists() {
                continue;
            }

            fetch_and_extract(&self.cache_client, basename, &self.cache_dir).await?;
        }

        Ok(())
    }

    /// Clean up incomplete temporary directories from previous crashes.
    pub fn cleanup_temp_dirs(&self) {
        let Ok(entries) = std::fs::read_dir(&self.cache_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with(".tmp-") {
                info!(path = %entry.path().display(), "removing stale temp directory");
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
}

/// Fetch a NAR from binary cache and extract it to the cache directory.
async fn fetch_and_extract(
    client: &impl BinaryCacheClient,
    basename: &str,
    cache_dir: &Path,
) -> Result<(), NiphasError> {
    info!(basename, "fetching from binary cache");

    let store_path = StorePath::parse_basename(basename)?;
    let narinfo = client.fetch_narinfo_by_hash(&store_path.hash_str()).await?;
    let nar_data = client.fetch_nar(&narinfo).await?;

    let file_hash = sha256_hash(&nar_data);
    if file_hash != narinfo.file_hash {
        return Err(NiphasError::HashMismatch {
            expected: narinfo.file_hash.to_string(),
            actual: file_hash.to_string(),
        });
    }

    let decompressed = decompress_nar(&narinfo, &nar_data).await?;

    let tmp_name = format!(".tmp-{}-{}", basename, std::process::id());
    let tmp_dir = cache_dir.join(&tmp_name);
    let final_dir = cache_dir.join(basename);

    std::fs::create_dir_all(&tmp_dir)?;

    let cursor = tokio::io::BufReader::new(&decompressed[..]);
    let reader = NarReader::new(cursor);
    let (node, nar_hash) = reader.parse().await.map_err(|e| {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        e
    })?;

    if nar_hash != narinfo.nar_hash {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(NiphasError::HashMismatch {
            expected: narinfo.nar_hash.to_string(),
            actual: nar_hash.to_string(),
        });
    }

    extract_node(&node, &tmp_dir)?;

    std::fs::rename(&tmp_dir, &final_dir).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    })?;

    info!(basename, "extracted to cache");
    Ok(())
}

/// Decompress NAR data based on narinfo compression type.
async fn decompress_nar(narinfo: &NarInfo, data: &[u8]) -> Result<Vec<u8>, NiphasError> {
    use niphas_core::nix::narinfo::Compression;

    macro_rules! decode {
        ($decoder:ty, $data:expr) => {{
            let decoder = <$decoder>::new(tokio::io::BufReader::new($data));
            let mut output = Vec::new();
            tokio::pin!(decoder);
            decoder.read_to_end(&mut output).await?;
            Ok(output)
        }};
    }

    match narinfo.compression {
        Compression::None => Ok(data.to_vec()),
        Compression::Zstd => decode!(async_compression::tokio::bufread::ZstdDecoder<_>, data),
        Compression::Xz => decode!(async_compression::tokio::bufread::XzDecoder<_>, data),
        Compression::Bzip2 => decode!(async_compression::tokio::bufread::BzDecoder<_>, data),
        other => Err(NiphasError::Cache(format!(
            "unsupported compression: {other}"
        ))),
    }
}

/// Extract a NAR node tree to a filesystem directory.
fn extract_node(
    node: &niphas_core::nix::nar::NarNode,
    path: &Path,
) -> Result<(), NiphasError> {
    use niphas_core::nix::nar::NarNode;

    match node {
        NarNode::Regular { executable, contents } => {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(path, contents)?;
            let mode = if *executable { 0o555 } else { 0o444 };
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
        }
        NarNode::Symlink { target } => {
            std::os::unix::fs::symlink(target, path)?;
        }
        NarNode::Directory { entries } => {
            if !path.exists() {
                std::fs::create_dir_all(path)?;
            }
            for entry in entries {
                let child_path = path.join(&entry.name);
                extract_node(&entry.node, &child_path)?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use niphas_core::testutils::fake_cache::FakeCacheClient;

    #[test]
    fn test_path_for_with_prefix() {
        let cache = NarCache::with_client(PathBuf::from("/cache"), FakeCacheClient::new());
        assert_eq!(
            cache.path_for("/nix/store/abc123-hello"),
            "/cache/abc123-hello"
        );
    }

    #[test]
    fn test_path_for_without_prefix() {
        let cache = NarCache::with_client(PathBuf::from("/cache"), FakeCacheClient::new());
        assert_eq!(cache.path_for("abc123-hello"), "/cache/abc123-hello");
    }
}
