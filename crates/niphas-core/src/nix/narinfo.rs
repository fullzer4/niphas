use crate::error::NiphasError;
use crate::nix::hash::NixHash;
use crate::nix::signature::NarSignature;
use crate::nix::store_path::{StorePath, StorePathRef};
use std::fmt;

/// Parsed `.narinfo` file from a binary cache.
#[derive(Debug, Clone, PartialEq)]
pub struct NarInfo {
    pub store_path: StorePath,
    pub url: String,
    pub compression: Compression,
    pub file_hash: NixHash,
    pub file_size: u64,
    pub nar_hash: NixHash,
    pub nar_size: u64,
    pub references: Vec<StorePathRef>,
    pub deriver: Option<String>,
    pub signatures: Vec<NarSignature>,
    pub ca: Option<String>,
}

/// Compression method for NAR files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    Xz,
    Bzip2,
    Zstd,
    Br,
    Lzip,
    Lz4,
}

impl Compression {
    pub fn parse(s: &str) -> Result<Self, NiphasError> {
        match s {
            "none" => Ok(Compression::None),
            "xz" => Ok(Compression::Xz),
            "bzip2" => Ok(Compression::Bzip2),
            "zstd" => Ok(Compression::Zstd),
            "br" => Ok(Compression::Br),
            "lzip" => Ok(Compression::Lzip),
            "lz4" => Ok(Compression::Lz4),
            _ => Err(NiphasError::NarInfoParse(format!(
                "unknown compression: '{s}'"
            ))),
        }
    }
}

impl fmt::Display for Compression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Compression::None => write!(f, "none"),
            Compression::Xz => write!(f, "xz"),
            Compression::Bzip2 => write!(f, "bzip2"),
            Compression::Zstd => write!(f, "zstd"),
            Compression::Br => write!(f, "br"),
            Compression::Lzip => write!(f, "lzip"),
            Compression::Lz4 => write!(f, "lz4"),
        }
    }
}

impl NarInfo {
    /// Parse a `.narinfo` text file.
    pub fn parse(input: &str) -> Result<Self, NiphasError> {
        let mut store_path: Option<StorePath> = None;
        let mut url: Option<String> = None;
        let mut compression: Option<Compression> = None;
        let mut file_hash: Option<NixHash> = None;
        let mut file_size: Option<u64> = None;
        let mut nar_hash: Option<NixHash> = None;
        let mut nar_size: Option<u64> = None;
        let mut references: Vec<StorePathRef> = Vec::new();
        let mut deriver: Option<String> = None;
        let mut signatures: Vec<NarSignature> = Vec::new();
        let mut ca: Option<String> = None;

        for line in input.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let (key, value) = line
                .split_once(": ")
                .ok_or_else(|| NiphasError::NarInfoParse(format!("malformed line: '{line}'")))?;

            match key {
                "StorePath" => {
                    store_path = Some(StorePath::parse(value)?);
                }
                "URL" => {
                    url = Some(value.to_owned());
                }
                "Compression" => {
                    compression = Some(Compression::parse(value)?);
                }
                "FileHash" => {
                    file_hash = Some(NixHash::parse(value)?);
                }
                "FileSize" => {
                    file_size = Some(value.parse::<u64>().map_err(|e| {
                        NiphasError::NarInfoParse(format!("invalid FileSize: {e}"))
                    })?);
                }
                "NarHash" => {
                    nar_hash = Some(NixHash::parse(value)?);
                }
                "NarSize" => {
                    nar_size = Some(value.parse::<u64>().map_err(|e| {
                        NiphasError::NarInfoParse(format!("invalid NarSize: {e}"))
                    })?);
                }
                "References" => {
                    if !value.is_empty() {
                        references = value
                            .split_whitespace()
                            .map(StorePathRef::parse)
                            .collect::<Result<Vec<_>, _>>()?;
                    }
                }
                "Deriver" => {
                    deriver = Some(value.to_owned());
                }
                "Sig" => {
                    signatures.push(NarSignature::parse(value)?);
                }
                "CA" => {
                    ca = Some(value.to_owned());
                }
                _ => {
                    // Ignore unknown fields for forward compatibility
                }
            }
        }

        let store_path =
            store_path.ok_or_else(|| NiphasError::NarInfoParse("missing StorePath".into()))?;
        let url = url.ok_or_else(|| NiphasError::NarInfoParse("missing URL".into()))?;
        let compression =
            compression.ok_or_else(|| NiphasError::NarInfoParse("missing Compression".into()))?;
        let file_hash =
            file_hash.ok_or_else(|| NiphasError::NarInfoParse("missing FileHash".into()))?;
        let file_size =
            file_size.ok_or_else(|| NiphasError::NarInfoParse("missing FileSize".into()))?;
        let nar_hash =
            nar_hash.ok_or_else(|| NiphasError::NarInfoParse("missing NarHash".into()))?;
        let nar_size =
            nar_size.ok_or_else(|| NiphasError::NarInfoParse("missing NarSize".into()))?;

        Ok(NarInfo {
            store_path,
            url,
            compression,
            file_hash,
            file_size,
            nar_hash,
            nar_size,
            references,
            deriver,
            signatures,
            ca,
        })
    }

    /// Compute the fingerprint string that signatures cover.
    ///
    /// Format: `1;<store-path>;<nar-hash>;<nar-size>;<comma-separated-sorted-references>`
    pub fn fingerprint(&self) -> String {
        let mut refs: Vec<String> = self
            .references
            .iter()
            .map(|r| r.to_store_path_string())
            .collect();
        refs.sort();
        let refs_str = refs.join(",");

        format!(
            "1;{};{};{};{}",
            self.store_path,
            self.nar_hash.to_string(),
            self.nar_size,
            refs_str
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_NARINFO: &str = "\
StorePath: /nix/store/00bgd045z0d4icpbc2yyz4gx48ak44la-net-tools-2.10
URL: nar/0i6vphc9vnrfamvnwwb07rml60k3piqf8samh4pzlzcslwdg19bv.nar.xz
Compression: xz
FileHash: sha256:0i6vphc9vnrfamvnwwb07rml60k3piqf8samh4pzlzcslwdg19bv
FileSize: 78656
NarHash: sha256:18q15y2rlpyar84fl0yh27c5b7n8xqsa7n7a7hc7183md2v9k73s
NarSize: 204840
References: 00bgd045z0d4icpbc2yyz4gx48ak44la-net-tools-2.10 3n58xw4373jp0ljirf06d8077j15pc4j-glibc-2.37-8
Sig: cache.nixos.org-1:tPtJYPW0S7siMoEqP85L2GMl44GVDBR2JFGBkUAjS+iCT1SQmyxs3JmfrvfNS5FCr7VIY+PF1sC+hJ3BL0lVDg==";

    #[test]
    fn test_parse_narinfo() {
        let info = NarInfo::parse(SAMPLE_NARINFO).unwrap();
        assert_eq!(info.store_path.name, "net-tools-2.10");
        assert_eq!(
            info.url,
            "nar/0i6vphc9vnrfamvnwwb07rml60k3piqf8samh4pzlzcslwdg19bv.nar.xz"
        );
        assert_eq!(info.compression, Compression::Xz);
        assert_eq!(info.file_size, 78656);
        assert_eq!(info.nar_size, 204840);
        assert_eq!(info.references.len(), 2);
        assert_eq!(info.signatures.len(), 1);
        assert_eq!(info.signatures[0].key_name, "cache.nixos.org-1");
    }

    #[test]
    fn test_missing_required_field() {
        let input = "StorePath: /nix/store/00bgd045z0d4icpbc2yyz4gx48ak44la-net-tools-2.10\n";
        assert!(NarInfo::parse(input).is_err());
    }

    #[test]
    fn test_fingerprint() {
        let info = NarInfo::parse(SAMPLE_NARINFO).unwrap();
        let fp = info.fingerprint();
        assert!(fp.starts_with("1;/nix/store/"));
        assert!(fp.contains(";sha256:"));
        assert!(fp.contains(";204840;"));
    }
}
