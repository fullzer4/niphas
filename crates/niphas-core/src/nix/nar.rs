use crate::error::NiphasError;
use crate::nix::hash::NixHash;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};

/// Magic string at the start of every NAR archive.
const NAR_MAGIC: &str = "nix-archive-1";

/// Default maximum nesting depth for directory entries.
const DEFAULT_MAX_DEPTH: u16 = 256;

/// Token-level reader over an AsyncRead stream.
///
/// Reads NAR wire primitives (u64, strings, padding) while computing
/// SHA-256 hash inline. Never loads the entire NAR into memory.
pub struct NarReader<R> {
    inner: R,
    depth: u16,
    max_depth: u16,
    bytes_read: u64,
    hasher: Sha256,
}

/// A parsed NAR node.
#[derive(Debug, PartialEq)]
pub enum NarNode {
    Regular { executable: bool, contents: Vec<u8> },
    Symlink { target: String },
    Directory { entries: Vec<NarEntry> },
}

/// A directory entry in a NAR archive.
#[derive(Debug, PartialEq)]
pub struct NarEntry {
    pub name: String,
    pub node: NarNode,
}

impl<R: AsyncRead + Unpin + Send> NarReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            depth: 0,
            max_depth: DEFAULT_MAX_DEPTH,
            bytes_read: 0,
            hasher: Sha256::new(),
        }
    }

    pub fn with_max_depth(mut self, max_depth: u16) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Read exactly `n` bytes, updating hash and byte count.
    async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), NiphasError> {
        self.inner
            .read_exact(buf)
            .await
            .map_err(|e| NiphasError::NarParse(format!("unexpected EOF: {e}")))?;
        self.hasher.update(&*buf);
        self.bytes_read += buf.len() as u64;
        Ok(())
    }

    /// Read a little-endian u64.
    async fn read_u64(&mut self) -> Result<u64, NiphasError> {
        let mut buf = [0u8; 8];
        self.read_exact(&mut buf).await?;
        Ok(u64::from_le_bytes(buf))
    }

    /// Read a length-prefixed byte string with padding.
    async fn read_bytes(&mut self) -> Result<Vec<u8>, NiphasError> {
        let len = self.read_u64().await?;
        if len > 128 * 1024 * 1024 {
            return Err(NiphasError::NarParse(format!(
                "string too large: {len} bytes"
            )));
        }
        let mut data = vec![0u8; len as usize];
        self.read_exact(&mut data).await?;

        // Read padding (0..7 bytes to reach 8-byte alignment)
        let pad_len = (8 - (len % 8)) % 8;
        if pad_len > 0 {
            let mut pad = vec![0u8; pad_len as usize];
            self.read_exact(&mut pad).await?;
            if pad.iter().any(|&b| b != 0) {
                return Err(NiphasError::NarParse("non-zero padding bytes".into()));
            }
        }

        Ok(data)
    }

    /// Read a length-prefixed UTF-8 string.
    async fn read_str(&mut self) -> Result<String, NiphasError> {
        let bytes = self.read_bytes().await?;
        String::from_utf8(bytes)
            .map_err(|e| NiphasError::NarParse(format!("invalid UTF-8 string: {e}")))
    }

    /// Read a string and verify it matches the expected value.
    async fn expect_str(&mut self, expected: &str) -> Result<(), NiphasError> {
        let s = self.read_str().await?;
        if s != expected {
            return Err(NiphasError::NarParse(format!(
                "expected '{expected}', got '{s}'"
            )));
        }
        Ok(())
    }

    /// Parse the entire NAR archive. Returns the root node and the NAR hash.
    pub async fn parse(mut self) -> Result<(NarNode, NixHash), NiphasError> {
        // Read magic
        self.expect_str(NAR_MAGIC).await?;

        // Parse root node
        let node = self.parse_node().await?;

        // Finalize hash
        let digest = self.hasher.finalize().to_vec();
        let nar_hash = NixHash {
            algo: crate::nix::hash::HashAlgo::Sha256,
            digest,
        };

        Ok((node, nar_hash))
    }

    /// Parse a single node: `"(" type_tag node_body ")"`.
    ///
    /// Per the NAR spec, a node is `str("(") nar-obj-inner str(")")`.
    /// For directory nodes, `parse_directory()` already consumes the
    /// closing `")"` (it reads `"entry"` or `")"` in a loop to detect
    /// end-of-directory), so we must not read it again.
    async fn parse_node(&mut self) -> Result<NarNode, NiphasError> {
        self.expect_str("(").await?;
        self.expect_str("type").await?;

        let type_str = self.read_str().await?;
        match type_str.as_str() {
            "regular" => {
                let node = self.parse_regular().await?;
                self.expect_str(")").await?;
                Ok(node)
            }
            "symlink" => {
                let node = self.parse_symlink().await?;
                self.expect_str(")").await?;
                Ok(node)
            }
            "directory" => {
                // parse_directory already consumes the closing ")"
                self.parse_directory().await
            }
            _ => Err(NiphasError::NarParse(format!(
                "unknown node type: '{type_str}'"
            ))),
        }
    }

    /// Parse a regular file node.
    async fn parse_regular(&mut self) -> Result<NarNode, NiphasError> {
        // Check for optional "executable" ""
        let next = self.read_str().await?;
        let (executable, contents) = if next == "executable" {
            self.expect_str("").await?;
            self.expect_str("contents").await?;
            (true, self.read_bytes().await?)
        } else if next == "contents" {
            (false, self.read_bytes().await?)
        } else {
            return Err(NiphasError::NarParse(format!(
                "expected 'executable' or 'contents', got '{next}'"
            )));
        };

        Ok(NarNode::Regular {
            executable,
            contents,
        })
    }

    /// Parse a symlink node.
    async fn parse_symlink(&mut self) -> Result<NarNode, NiphasError> {
        self.expect_str("target").await?;
        let target = self.read_str().await?;
        Ok(NarNode::Symlink { target })
    }

    /// Parse a directory node with sorted entries.
    async fn parse_directory(&mut self) -> Result<NarNode, NiphasError> {
        self.depth += 1;
        if self.depth > self.max_depth {
            return Err(NiphasError::NarParse(format!(
                "directory nesting depth exceeds limit ({})",
                self.max_depth
            )));
        }

        let mut entries = Vec::new();
        let mut last_name: Option<String> = None;

        loop {
            // Peek next string -- either "entry" or ")"
            let next = self.read_str().await?;
            if next == ")" {
                // Directory consumes its own closing ")" since we can't unread tokens.
                // The caller (parse_node_inner) skips expect_str(")") for directories.
                self.depth -= 1;
                return Ok(NarNode::Directory { entries });
            }

            if next != "entry" {
                return Err(NiphasError::NarParse(format!(
                    "expected 'entry' or ')', got '{next}'"
                )));
            }

            // Parse entry: "(" "name" str "node" node ")"
            self.expect_str("(").await?;
            self.expect_str("name").await?;
            let name = self.read_str().await?;

            // Validate entry name
            validate_entry_name(&name)?;

            // Check ordering
            if let Some(ref prev) = last_name {
                if name <= *prev {
                    return Err(NiphasError::NarParse(format!(
                        "directory entries not sorted: '{prev}' followed by '{name}'"
                    )));
                }
            }
            last_name = Some(name.clone());

            self.expect_str("node").await?;
            let node = self.parse_node_inner().await?;
            self.expect_str(")").await?;

            entries.push(NarEntry { name, node });
        }
    }

    /// Inner parse that always reads closing ")".
    /// Boxed to break the async recursion cycle (directory -> entry -> node -> directory).
    fn parse_node_inner(
        &mut self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<NarNode, NiphasError>> + Send + '_>,
    > {
        Box::pin(async move {
            self.expect_str("(").await?;
            self.expect_str("type").await?;

            let type_str = self.read_str().await?;
            let node = match type_str.as_str() {
                "regular" => {
                    let n = self.parse_regular().await?;
                    self.expect_str(")").await?;
                    n
                }
                "symlink" => {
                    let n = self.parse_symlink().await?;
                    self.expect_str(")").await?;
                    n
                }
                "directory" => {
                    // parse_directory already consumes the closing ")"
                    self.parse_directory().await?
                }
                _ => {
                    return Err(NiphasError::NarParse(format!(
                        "unknown node type: '{type_str}'"
                    )));
                }
            };

            Ok(node)
        })
    }

    /// Get the total bytes read from the stream.
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
}

/// Validate a directory entry name per NAR spec.
fn validate_entry_name(name: &str) -> Result<(), NiphasError> {
    if name.is_empty() {
        return Err(NiphasError::NarParse("empty entry name".into()));
    }
    if name == "." || name == ".." {
        return Err(NiphasError::NarParse(format!(
            "invalid entry name: '{name}'"
        )));
    }
    if name.contains('/') || name.contains('\0') {
        return Err(NiphasError::NarParse(format!(
            "entry name contains forbidden characters: '{name}'"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: encode a NAR string token (length-prefixed + padded).
    fn nar_str(s: &str) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(s.len() as u64).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
        let pad = (8 - (s.len() % 8)) % 8;
        out.extend(std::iter::repeat_n(0u8, pad));
        out
    }

    /// Build a minimal NAR for a regular file.
    fn build_regular_nar(contents: &[u8], executable: bool) -> Vec<u8> {
        let mut nar = Vec::new();
        nar.extend(nar_str("nix-archive-1"));
        nar.extend(nar_str("("));
        nar.extend(nar_str("type"));
        nar.extend(nar_str("regular"));
        if executable {
            nar.extend(nar_str("executable"));
            nar.extend(nar_str(""));
        }
        nar.extend(nar_str("contents"));
        // contents is encoded like a string
        nar.extend(&(contents.len() as u64).to_le_bytes());
        nar.extend(contents);
        let pad = (8 - (contents.len() % 8)) % 8;
        nar.extend(std::iter::repeat_n(0u8, pad));
        nar.extend(nar_str(")"));
        nar
    }

    #[tokio::test]
    async fn test_parse_regular_file() {
        let nar = build_regular_nar(b"hello world", false);
        let reader = NarReader::new(&nar[..]);
        let (node, hash) = reader.parse().await.unwrap();

        match node {
            NarNode::Regular {
                executable,
                contents,
            } => {
                assert!(!executable);
                assert_eq!(contents, b"hello world");
            }
            _ => panic!("expected regular file"),
        }

        // Hash should be valid SHA-256
        assert_eq!(hash.algo, crate::nix::hash::HashAlgo::Sha256);
        assert_eq!(hash.digest.len(), 32);
    }

    #[tokio::test]
    async fn test_parse_executable_file() {
        let nar = build_regular_nar(b"#!/bin/sh\necho hi", true);
        let reader = NarReader::new(&nar[..]);
        let (node, _) = reader.parse().await.unwrap();

        match node {
            NarNode::Regular { executable, .. } => {
                assert!(executable);
            }
            _ => panic!("expected regular file"),
        }
    }

    #[tokio::test]
    async fn test_parse_symlink() {
        let mut nar = Vec::new();
        nar.extend(nar_str("nix-archive-1"));
        nar.extend(nar_str("("));
        nar.extend(nar_str("type"));
        nar.extend(nar_str("symlink"));
        nar.extend(nar_str("target"));
        nar.extend(nar_str("/nix/store/abc-glibc/lib/libc.so.6"));
        nar.extend(nar_str(")"));

        let reader = NarReader::new(&nar[..]);
        let (node, _) = reader.parse().await.unwrap();

        match node {
            NarNode::Symlink { target } => {
                assert_eq!(target, "/nix/store/abc-glibc/lib/libc.so.6");
            }
            _ => panic!("expected symlink"),
        }
    }

    /// Build a NAR for a directory with the given entries.
    /// Each entry is (name, nar_node_bytes) where nar_node_bytes is
    /// the inner serialization of a node (including "(" ... ")").
    fn build_directory_nar(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut nar = Vec::new();
        nar.extend(nar_str("nix-archive-1"));
        nar.extend(nar_str("("));
        nar.extend(nar_str("type"));
        nar.extend(nar_str("directory"));
        for (name, node_bytes) in entries {
            nar.extend(nar_str("entry"));
            nar.extend(nar_str("("));
            nar.extend(nar_str("name"));
            nar.extend(nar_str(name));
            nar.extend(nar_str("node"));
            nar.extend(node_bytes);
            nar.extend(nar_str(")"));
        }
        nar.extend(nar_str(")"));
        nar
    }

    /// Build the inner NAR bytes for a regular file node (including parens).
    fn build_regular_node(contents: &[u8]) -> Vec<u8> {
        let mut node = Vec::new();
        node.extend(nar_str("("));
        node.extend(nar_str("type"));
        node.extend(nar_str("regular"));
        node.extend(nar_str("contents"));
        node.extend(&(contents.len() as u64).to_le_bytes());
        node.extend(contents);
        let pad = (8 - (contents.len() % 8)) % 8;
        node.extend(std::iter::repeat_n(0u8, pad));
        node.extend(nar_str(")"));
        node
    }

    #[tokio::test]
    async fn test_parse_empty_directory() {
        let nar = build_directory_nar(&[]);
        let reader = NarReader::new(&nar[..]);
        let (node, hash) = reader.parse().await.unwrap();

        match node {
            NarNode::Directory { entries } => {
                assert!(entries.is_empty());
            }
            _ => panic!("expected directory"),
        }
        assert_eq!(hash.algo, crate::nix::hash::HashAlgo::Sha256);
        assert_eq!(hash.digest.len(), 32);
    }

    #[tokio::test]
    async fn test_parse_directory_with_files() {
        let nar = build_directory_nar(&[
            ("hello.txt", build_regular_node(b"hello")),
            ("world.txt", build_regular_node(b"world")),
        ]);
        let reader = NarReader::new(&nar[..]);
        let (node, _) = reader.parse().await.unwrap();

        match node {
            NarNode::Directory { entries } => {
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].name, "hello.txt");
                assert_eq!(entries[1].name, "world.txt");
                match &entries[0].node {
                    NarNode::Regular { contents, .. } => assert_eq!(contents, b"hello"),
                    _ => panic!("expected regular file"),
                }
            }
            _ => panic!("expected directory"),
        }
    }

    #[tokio::test]
    async fn test_parse_nested_directory() {
        // Build: root/subdir/file.txt
        let inner_dir = {
            let file_node = build_regular_node(b"nested content");
            let mut d = Vec::new();
            d.extend(nar_str("("));
            d.extend(nar_str("type"));
            d.extend(nar_str("directory"));
            d.extend(nar_str("entry"));
            d.extend(nar_str("("));
            d.extend(nar_str("name"));
            d.extend(nar_str("file.txt"));
            d.extend(nar_str("node"));
            d.extend(&file_node);
            d.extend(nar_str(")"));
            d.extend(nar_str(")"));
            d
        };

        let nar = build_directory_nar(&[("subdir", inner_dir)]);
        let reader = NarReader::new(&nar[..]);
        let (node, _) = reader.parse().await.unwrap();

        match node {
            NarNode::Directory { entries } => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].name, "subdir");
                match &entries[0].node {
                    NarNode::Directory { entries: sub } => {
                        assert_eq!(sub.len(), 1);
                        assert_eq!(sub[0].name, "file.txt");
                    }
                    _ => panic!("expected nested directory"),
                }
            }
            _ => panic!("expected directory"),
        }
    }

    #[test]
    fn test_validate_entry_name() {
        assert!(validate_entry_name("hello").is_ok());
        assert!(validate_entry_name("").is_err());
        assert!(validate_entry_name(".").is_err());
        assert!(validate_entry_name("..").is_err());
        assert!(validate_entry_name("foo/bar").is_err());
        assert!(validate_entry_name("foo\0bar").is_err());
    }
}
