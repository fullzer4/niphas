use crate::error::NiphasError;
use sha2::{Digest, Sha256};
use std::fmt;

/// Nix-base32 alphabet: `0123456789abcdfghijklmnpqrsvwxyz` (omits e, o, t, u).
const NIX_BASE32_CHARS: &[u8; 32] = b"0123456789abcdfghijklmnpqrsvwxyz";

/// Lookup table for decoding: maps ASCII byte -> 5-bit value (255 = invalid).
const NIX_BASE32_DECODE: [u8; 128] = {
    let mut table = [255u8; 128];
    let mut i = 0;
    while i < 32 {
        table[NIX_BASE32_CHARS[i] as usize] = i as u8;
        i += 1;
    }
    table
};

/// Encode bytes to Nix-base32 string.
///
/// Nix-base32 is **reversed** compared to standard base32: the least
/// significant bits come first in the encoded output, but the output
/// string is reversed so the MSB appears first.
pub fn to_nix_base32(input: &[u8]) -> String {
    if input.is_empty() {
        return String::new();
    }
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

/// Decode a Nix-base32 string back to bytes.
pub fn from_nix_base32(input: &str) -> Result<Vec<u8>, NiphasError> {
    let len = input.len();
    if len == 0 {
        return Ok(vec![]);
    }

    // Number of output bytes: floor(len * 5 / 8)
    let out_len = len * 5 / 8;
    let mut out = vec![0u8; out_len];

    for (i, ch) in input.chars().rev().enumerate() {
        let ch_byte = ch as usize;
        if ch_byte >= 128 {
            return Err(NiphasError::NarInfoParse(format!(
                "invalid nix-base32 character: '{ch}'"
            )));
        }
        let digit = NIX_BASE32_DECODE[ch_byte];
        if digit == 255 {
            return Err(NiphasError::NarInfoParse(format!(
                "invalid nix-base32 character: '{ch}'"
            )));
        }

        let b = i * 5;
        let byte_idx = b / 8;
        let bit_idx = b % 8;

        out[byte_idx] |= digit << bit_idx;
        if bit_idx > 3 && byte_idx + 1 < out_len {
            out[byte_idx + 1] |= digit >> (8 - bit_idx);
        }
    }

    Ok(out)
}

/// Hash algorithm used by Nix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashAlgo {
    Sha256,
    Sha512,
    Sha1,
    Md5,
}

impl HashAlgo {
    pub fn digest_len(&self) -> usize {
        match self {
            HashAlgo::Sha256 => 32,
            HashAlgo::Sha512 => 64,
            HashAlgo::Sha1 => 20,
            HashAlgo::Md5 => 16,
        }
    }
}

impl fmt::Display for HashAlgo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HashAlgo::Sha256 => write!(f, "sha256"),
            HashAlgo::Sha512 => write!(f, "sha512"),
            HashAlgo::Sha1 => write!(f, "sha1"),
            HashAlgo::Md5 => write!(f, "md5"),
        }
    }
}

/// A Nix hash: algorithm + raw digest bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NixHash {
    pub algo: HashAlgo,
    pub digest: Vec<u8>,
}

impl NixHash {
    /// Parse `"sha256:1impfh..."` (algo:nix-base32) format.
    pub fn parse(s: &str) -> Result<Self, NiphasError> {
        let (algo_str, hash_str) = s
            .split_once(':')
            .ok_or_else(|| NiphasError::NarInfoParse("hash missing ':' separator".into()))?;

        let algo = match algo_str {
            "sha256" => HashAlgo::Sha256,
            "sha512" => HashAlgo::Sha512,
            "sha1" => HashAlgo::Sha1,
            "md5" => HashAlgo::Md5,
            _ => {
                return Err(NiphasError::NarInfoParse(format!(
                    "unknown hash algorithm: '{algo_str}'"
                )));
            }
        };

        let digest = from_nix_base32(hash_str)?;

        let expected_len = algo.digest_len();
        if digest.len() != expected_len {
            return Err(NiphasError::NarInfoParse(format!(
                "hash digest length mismatch: expected {expected_len}, got {}",
                digest.len()
            )));
        }

        Ok(NixHash { algo, digest })
    }

    /// Verify that `data` matches this hash.
    pub fn verify(&self, data: &[u8]) -> Result<(), NiphasError> {
        let computed = match self.algo {
            HashAlgo::Sha256 => Sha256::digest(data).to_vec(),
            _ => {
                return Err(NiphasError::NarInfoParse(format!(
                    "verification not implemented for {}",
                    self.algo
                )));
            }
        };

        if computed != self.digest {
            return Err(NiphasError::HashMismatch {
                expected: to_nix_base32(&self.digest),
                actual: to_nix_base32(&computed),
            });
        }
        Ok(())
    }
}

impl fmt::Display for NixHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.algo, to_nix_base32(&self.digest))
    }
}

/// Compute SHA-256 of data and return as NixHash.
pub fn sha256_hash(data: &[u8]) -> NixHash {
    let digest = Sha256::digest(data).to_vec();
    NixHash {
        algo: HashAlgo::Sha256,
        digest,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nix_base32_roundtrip() {
        let data = b"hello world";
        let encoded = to_nix_base32(data);
        let decoded = from_nix_base32(&encoded).unwrap();
        assert_eq!(data.as_slice(), &decoded);
    }

    #[test]
    fn test_nix_base32_empty() {
        assert_eq!(to_nix_base32(b""), "");
        assert_eq!(from_nix_base32("").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn test_nix_base32_known_vector() {
        // SHA-256 of empty string = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        let sha256_empty = [
            0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
            0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
            0x78, 0x52, 0xb8, 0x55,
        ];
        let encoded = to_nix_base32(&sha256_empty);
        // Should be 52 chars (32 * 8 / 5 = 51.2, ceil = 52)
        assert_eq!(encoded.len(), 52);
        let decoded = from_nix_base32(&encoded).unwrap();
        assert_eq!(decoded, sha256_empty);
    }

    #[test]
    fn test_nix_hash_parse() {
        // Generate a valid nix-base32 encoded SHA-256 hash
        let hash = sha256_hash(b"test");
        let nix_str = hash.to_string();
        let parsed = NixHash::parse(&nix_str).unwrap();
        assert_eq!(parsed.algo, HashAlgo::Sha256);
        assert_eq!(parsed.digest.len(), 32);
        assert_eq!(parsed, hash);
    }

    #[test]
    fn test_nix_hash_roundtrip() {
        let original = sha256_hash(b"test data");
        let s = original.to_string();
        let parsed = NixHash::parse(&s).unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn test_invalid_base32_char() {
        assert!(from_nix_base32("hello!").is_err());
    }

    #[test]
    fn test_hash_verify() {
        let data = b"hello";
        let hash = sha256_hash(data);
        assert!(hash.verify(data).is_ok());
        assert!(hash.verify(b"world").is_err());
    }
}
