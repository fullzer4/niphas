use crate::error::NiphasError;
use crate::nix::hash::{from_nix_base32, to_nix_base32};
use std::fmt;

/// Nix store path prefix.
pub const STORE_DIR: &str = "/nix/store";

/// Length of the hash part in a store path (Nix-base32 encoded).
const HASH_LEN: usize = 32;

/// A parsed Nix store path: `/nix/store/<hash>-<name>`.
///
/// The hash is 20 bytes (truncated SHA-256), encoded as 32 Nix-base32 chars.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StorePath {
    /// 20-byte hash (decoded from the 32-char Nix-base32 prefix).
    pub hash: [u8; 20],
    /// Package name with version, e.g. `hello-2.12.1`.
    pub name: String,
}

impl StorePath {
    /// Parse a full store path like `/nix/store/abc123...-hello-2.12.1`.
    pub fn parse(s: &str) -> Result<Self, NiphasError> {
        let rest = s
            .strip_prefix(STORE_DIR)
            .and_then(|s| s.strip_prefix('/'))
            .ok_or_else(|| {
                NiphasError::InvalidStorePath(format!("missing /nix/store/ prefix: '{s}'"))
            })?;

        Self::parse_basename(rest)
    }

    /// Parse a store path basename like `abc123...-hello-2.12.1` (no `/nix/store/` prefix).
    pub fn parse_basename(basename: &str) -> Result<Self, NiphasError> {
        // Minimum: 32 hash chars + "-" + 1 name char = 34
        if basename.len() < HASH_LEN + 2 {
            return Err(NiphasError::InvalidStorePath(format!(
                "store path basename too short: '{basename}'"
            )));
        }

        let hash_str = &basename[..HASH_LEN];
        if basename.as_bytes()[HASH_LEN] != b'-' {
            return Err(NiphasError::InvalidStorePath(format!(
                "expected '-' after hash in store path: '{basename}'"
            )));
        }
        let name = &basename[HASH_LEN + 1..];

        // Validate name
        if name.is_empty() {
            return Err(NiphasError::InvalidStorePath(
                "store path name is empty".into(),
            ));
        }

        let hash_bytes = from_nix_base32(hash_str)?;
        let hash: [u8; 20] = hash_bytes.try_into().map_err(|_| {
            NiphasError::InvalidStorePath(format!(
                "hash decode produced {} bytes, expected 20",
                hash_str.len()
            ))
        })?;

        Ok(StorePath {
            hash,
            name: name.to_owned(),
        })
    }

    /// The 32-char Nix-base32 hash string, used for `.narinfo` lookup.
    pub fn hash_str(&self) -> String {
        to_nix_base32(&self.hash)
    }

    /// The basename: `<hash>-<name>`.
    pub fn basename(&self) -> String {
        format!("{}-{}", self.hash_str(), self.name)
    }
}

impl fmt::Display for StorePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}-{}", STORE_DIR, self.hash_str(), self.name)
    }
}

/// A store path reference -- just the basename (e.g. `abc123-hello-2.12.1`).
/// Used in `.narinfo` References field where paths don't have the `/nix/store/` prefix.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StorePathRef(String);

impl StorePathRef {
    /// Parse a basename like `abc123-hello-2.12.1`.
    pub fn parse(s: &str) -> Result<Self, NiphasError> {
        // Validate it's a valid store path basename
        StorePath::parse_basename(s)?;
        Ok(StorePathRef(s.to_owned()))
    }

    /// Convert to a full store path string.
    pub fn to_store_path_string(&self) -> String {
        format!("{}/{}", STORE_DIR, self.0)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for StorePathRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_full_path() {
        let path = "/nix/store/00bgd045z0d4icpbc2yyz4gx48ak44la-net-tools-2.10";
        let sp = StorePath::parse(path).unwrap();
        assert_eq!(sp.name, "net-tools-2.10");
        assert_eq!(sp.to_string(), path);
    }

    #[test]
    fn test_parse_basename() {
        let basename = "00bgd045z0d4icpbc2yyz4gx48ak44la-net-tools-2.10";
        let sp = StorePath::parse_basename(basename).unwrap();
        assert_eq!(sp.name, "net-tools-2.10");
        assert_eq!(sp.basename(), basename);
    }

    #[test]
    fn test_invalid_prefix() {
        assert!(StorePath::parse("/usr/bin/hello").is_err());
    }

    #[test]
    fn test_too_short() {
        assert!(StorePath::parse("/nix/store/abc").is_err());
    }

    #[test]
    fn test_store_path_ref() {
        let r = StorePathRef::parse("00bgd045z0d4icpbc2yyz4gx48ak44la-net-tools-2.10").unwrap();
        assert_eq!(
            r.to_store_path_string(),
            "/nix/store/00bgd045z0d4icpbc2yyz4gx48ak44la-net-tools-2.10"
        );
    }

    #[test]
    fn test_hash_extraction() {
        let sp =
            StorePath::parse("/nix/store/00bgd045z0d4icpbc2yyz4gx48ak44la-net-tools-2.10").unwrap();
        let hash = sp.hash_str();
        assert_eq!(hash.len(), 32);
        assert_eq!(hash, "00bgd045z0d4icpbc2yyz4gx48ak44la");
    }

    #[test]
    fn test_roundtrip() {
        let original = "/nix/store/00bgd045z0d4icpbc2yyz4gx48ak44la-net-tools-2.10";
        let sp = StorePath::parse(original).unwrap();
        assert_eq!(sp.to_string(), original);
    }
}
