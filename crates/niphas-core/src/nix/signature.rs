use crate::error::NiphasError;
use base64::prelude::*;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

/// A signature from a `.narinfo` Sig field.
///
/// Format: `<keyname>:<base64-signature>`
#[derive(Debug, Clone, PartialEq)]
pub struct NarSignature {
    pub key_name: String,
    pub signature: Signature,
}

impl NarSignature {
    /// Parse a `Sig` field value like `cache.nixos.org-1:AAAA...base64...==`.
    pub fn parse(s: &str) -> Result<Self, NiphasError> {
        let (key_name, sig_b64) = s.split_once(':').ok_or_else(|| {
            NiphasError::NarInfoParse(format!("signature missing ':' separator: '{s}'"))
        })?;

        let sig_bytes = BASE64_STANDARD.decode(sig_b64).map_err(|e| {
            NiphasError::NarInfoParse(format!("invalid base64 in signature: {e}"))
        })?;

        let sig_array: [u8; 64] = sig_bytes.try_into().map_err(|_| {
            NiphasError::NarInfoParse("signature must be 64 bytes".into())
        })?;

        let signature = Signature::from_bytes(&sig_array);

        Ok(NarSignature {
            key_name: key_name.to_owned(),
            signature,
        })
    }
}

/// A trusted public key for verifying narinfo signatures.
///
/// Format: `cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=`
#[derive(Debug, Clone)]
pub struct TrustedKey {
    pub name: String,
    pub pubkey: VerifyingKey,
}

impl TrustedKey {
    /// Parse a public key string like `cache.nixos.org-1:6NCHdD...=`.
    pub fn parse(s: &str) -> Result<Self, NiphasError> {
        let (name, key_b64) = s.split_once(':').ok_or_else(|| {
            NiphasError::SignatureVerification(format!("key missing ':' separator: '{s}'"))
        })?;

        let key_bytes = BASE64_STANDARD.decode(key_b64).map_err(|e| {
            NiphasError::SignatureVerification(format!("invalid base64 in public key: {e}"))
        })?;

        let key_array: [u8; 32] = key_bytes.try_into().map_err(|_| {
            NiphasError::SignatureVerification("public key must be 32 bytes".into())
        })?;

        let pubkey = VerifyingKey::from_bytes(&key_array).map_err(|e| {
            NiphasError::SignatureVerification(format!("invalid Ed25519 public key: {e}"))
        })?;

        Ok(TrustedKey {
            name: name.to_owned(),
            pubkey,
        })
    }
}

/// Verify a narinfo's signatures against a set of trusted keys.
///
/// Returns Ok if at least one signature matches a trusted key.
/// Returns Err if no signature matches.
pub fn verify_narinfo(
    fingerprint: &str,
    signatures: &[NarSignature],
    trusted_keys: &[TrustedKey],
) -> Result<(), NiphasError> {
    let fingerprint_bytes = fingerprint.as_bytes();

    for sig in signatures {
        for key in trusted_keys {
            if sig.key_name == key.name {
                if key.pubkey.verify(fingerprint_bytes, &sig.signature).is_ok() {
                    return Ok(());
                }
            }
        }
    }

    Err(NiphasError::SignatureVerification(
        "no valid signature found matching any trusted key".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_signature() {
        let sig_str = "cache.nixos.org-1:tPtJYPW0S7siMoEqP85L2GMl44GVDBR2JFGBkUAjS+iCT1SQmyxs3JmfrvfNS5FCr7VIY+PF1sC+hJ3BL0lVDg==";
        let sig = NarSignature::parse(sig_str).unwrap();
        assert_eq!(sig.key_name, "cache.nixos.org-1");
    }

    #[test]
    fn test_parse_trusted_key() {
        let key_str = "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=";
        let key = TrustedKey::parse(key_str).unwrap();
        assert_eq!(key.name, "cache.nixos.org-1");
    }

    #[test]
    fn test_invalid_signature_format() {
        assert!(NarSignature::parse("no-colon-here").is_err());
    }

    #[test]
    fn test_invalid_key_format() {
        assert!(TrustedKey::parse("no-colon-here").is_err());
    }

    #[test]
    fn test_wrong_key_length() {
        // Too short base64
        assert!(TrustedKey::parse("test:AAAA").is_err());
    }
}
