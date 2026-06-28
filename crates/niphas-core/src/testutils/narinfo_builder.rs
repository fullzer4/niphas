//! Builder for constructing NarInfo test fixtures.

use crate::nix::hash::{HashAlgo, NixHash};
use crate::nix::narinfo::{Compression, NarInfo};
use crate::nix::signature::NarSignature;
use crate::nix::store_path::{StorePath, StorePathRef};

/// Builder for creating NarInfo fixtures in tests.
///
/// ```rust,ignore
/// let narinfo = NarInfoBuilder::new("/nix/store/00bgd045z0d4icpbc2yyz4gx48ak44la-hello-2.12.1")
///     .references(vec!["3n58xw4373jp0ljirf06d8077j15pc4j-glibc-2.37-8"])
///     .nar_size(204840)
///     .build();
/// ```
pub struct NarInfoBuilder {
    store_path: StorePath,
    url: String,
    compression: Compression,
    file_hash: NixHash,
    file_size: u64,
    nar_hash: NixHash,
    nar_size: u64,
    references: Vec<StorePathRef>,
    signatures: Vec<NarSignature>,
}

impl NarInfoBuilder {
    /// Create a builder from a full store path string.
    ///
    /// Panics if the store path is invalid (test-only).
    pub fn new(store_path: &str) -> Self {
        let sp = StorePath::parse(store_path)
            .unwrap_or_else(|e| panic!("invalid store path '{store_path}': {e}"));
        let hash_str = sp.hash_str();
        Self {
            url: format!("nar/{hash_str}.nar.zst"),
            store_path: sp,
            compression: Compression::None,
            file_hash: NixHash {
                algo: HashAlgo::Sha256,
                digest: vec![0u8; 32],
            },
            file_size: 1000,
            nar_hash: NixHash {
                algo: HashAlgo::Sha256,
                digest: vec![0u8; 32],
            },
            nar_size: 2000,
            references: Vec::new(),
            signatures: Vec::new(),
        }
    }

    /// Set references (as basenames like `hash-name`).
    pub fn references(mut self, refs: Vec<&str>) -> Self {
        self.references = refs
            .into_iter()
            .map(|r| {
                StorePathRef::parse(r)
                    .unwrap_or_else(|e| panic!("invalid store path ref '{r}': {e}"))
            })
            .collect();
        self
    }

    pub fn nar_size(mut self, size: u64) -> Self {
        self.nar_size = size;
        self
    }

    pub fn file_size(mut self, size: u64) -> Self {
        self.file_size = size;
        self
    }

    pub fn compression(mut self, c: Compression) -> Self {
        self.compression = c;
        self
    }

    /// Set signatures on the narinfo.
    pub fn signatures(mut self, sigs: Vec<NarSignature>) -> Self {
        self.signatures = sigs;
        self
    }

    pub fn build(self) -> NarInfo {
        NarInfo {
            store_path: self.store_path,
            url: self.url,
            compression: self.compression,
            file_hash: self.file_hash,
            file_size: self.file_size,
            nar_hash: self.nar_hash,
            nar_size: self.nar_size,
            references: self.references,
            deriver: None,
            signatures: self.signatures,
            ca: None,
        }
    }
}
