use crate::error::NiphasError;
use serde::{Deserialize, Serialize};

/// Request body for the eval webhook (POST /evaluate).
///
/// Shared between niphas-operator (serializer) and niphas-eval (deserializer).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalRequest {
    pub flake_ref: String,
    pub attribute: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_cache: Option<String>,
}

/// Successful response from the eval webhook.
///
/// Shared between niphas-eval (serializer) and niphas-operator (deserializer).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalResponse {
    pub store_path: String,
    pub name: String,
    pub main_program: Option<String>,
    pub closure_paths: Vec<String>,
}

impl EvalResponse {
    /// Compute the resolved command path.
    pub fn resolved_command(&self) -> String {
        if let Some(ref prog) = self.main_program {
            format!("{}/bin/{}", self.store_path, prog)
        } else {
            format!("{}/bin/{}", self.store_path, self.name)
        }
    }
}

/// Validate a flake reference against injection-safe characters.
///
/// Allows: `scheme:path` where scheme is `[a-zA-Z][a-zA-Z0-9+\-\.]*`
/// and path is `[a-zA-Z0-9/_\-\.@:#+?=&~]+` (covers github:org/repo, path:./foo,
/// git+https://..., query params like ?dir=subdir&ref=main, etc.)
///
/// Rejects: `"`, `\`, `$`, `;`, `(`, `)`, spaces, backticks, and other
/// characters that could break out of a Nix string interpolation.
pub fn validate_flake_ref(flake_ref: &str) -> Result<(), NiphasError> {
    if flake_ref.is_empty() {
        return Err(NiphasError::InvalidInput(
            "flake_ref must not be empty".into(),
        ));
    }

    // Must have scheme:path structure
    let Some((scheme, path)) = flake_ref.split_once(':') else {
        return Err(NiphasError::InvalidInput(format!(
            "flake_ref missing scheme: '{flake_ref}'"
        )));
    };

    // Validate scheme: [a-zA-Z][a-zA-Z0-9+\-\.]*
    let mut scheme_chars = scheme.chars();
    match scheme_chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => {
            return Err(NiphasError::InvalidInput(format!(
                "flake_ref scheme must start with a letter: '{scheme}'"
            )));
        }
    }
    for c in scheme_chars {
        if !(c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.') {
            return Err(NiphasError::InvalidInput(format!(
                "flake_ref scheme contains invalid character '{c}': '{scheme}'"
            )));
        }
    }

    // Validate path: safe characters only
    if path.is_empty() {
        return Err(NiphasError::InvalidInput(format!(
            "flake_ref path must not be empty: '{flake_ref}'"
        )));
    }
    for c in path.chars() {
        if !(c.is_ascii_alphanumeric()
            || c == '/'
            || c == '_'
            || c == '-'
            || c == '.'
            || c == '@'
            || c == ':'
            || c == '#'
            || c == '+'
            || c == '?'
            || c == '='
            || c == '&'
            || c == '~')
        {
            return Err(NiphasError::InvalidInput(format!(
                "flake_ref contains unsafe character '{c}': '{flake_ref}'"
            )));
        }
    }

    Ok(())
}

/// Validate a Nix attribute path (e.g. `packages.x86_64-linux.default`).
///
/// Only allows dot-separated identifiers: `[a-zA-Z_][a-zA-Z0-9_\-]*`
pub fn validate_attribute(attribute: &str) -> Result<(), NiphasError> {
    if attribute.is_empty() {
        return Err(NiphasError::InvalidInput(
            "attribute must not be empty".into(),
        ));
    }

    for segment in attribute.split('.') {
        if segment.is_empty() {
            return Err(NiphasError::InvalidInput(format!(
                "attribute contains empty segment: '{attribute}'"
            )));
        }

        let mut chars = segment.chars();
        match chars.next() {
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
            Some(c) => {
                return Err(NiphasError::InvalidInput(format!(
                    "attribute segment must start with letter or '_', got '{c}': '{attribute}'"
                )));
            }
            None => unreachable!(), // already checked is_empty
        }

        for c in chars {
            if !(c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                return Err(NiphasError::InvalidInput(format!(
                    "attribute contains invalid character '{c}': '{attribute}'"
                )));
            }
        }
    }

    Ok(())
}

/// Validate a git revision (hex SHA, 6-40 characters).
pub fn validate_revision(revision: &str) -> Result<(), NiphasError> {
    if revision.len() < 6 || revision.len() > 40 {
        return Err(NiphasError::InvalidInput(format!(
            "revision must be 6-40 hex characters, got {} characters: '{revision}'",
            revision.len()
        )));
    }

    for c in revision.chars() {
        if !c.is_ascii_hexdigit() {
            return Err(NiphasError::InvalidInput(format!(
                "revision contains non-hex character '{c}': '{revision}'"
            )));
        }
    }

    Ok(())
}

/// Validate all fields of an `EvalRequest`.
pub fn validate_eval_request(req: &EvalRequest) -> Result<(), NiphasError> {
    validate_flake_ref(&req.flake_ref)?;
    validate_attribute(&req.attribute)?;
    if let Some(ref rev) = req.revision {
        validate_revision(rev)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- validate_flake_ref --

    #[test]
    fn valid_github_ref() {
        assert!(validate_flake_ref("github:myorg/myapp").is_ok());
    }

    #[test]
    fn valid_git_https_ref() {
        assert!(validate_flake_ref("git+https://github.com/org/repo").is_ok());
    }

    #[test]
    fn valid_path_ref() {
        assert!(validate_flake_ref("path:./my-flake").is_ok());
    }

    #[test]
    fn valid_ref_with_query_params() {
        assert!(validate_flake_ref("github:org/repo?dir=subdir&ref=main").is_ok());
    }

    #[test]
    fn valid_ref_with_tilde() {
        assert!(validate_flake_ref("path:~/my-flake").is_ok());
    }

    #[test]
    fn rejects_empty_flake_ref() {
        assert!(validate_flake_ref("").is_err());
    }

    #[test]
    fn rejects_no_scheme() {
        assert!(validate_flake_ref("just-a-path").is_err());
    }

    #[test]
    fn rejects_double_quote_injection() {
        assert!(
            validate_flake_ref(r#"github:org/app" ++ builtins.readFile /etc/shadow ++ ""#).is_err()
        );
    }

    #[test]
    fn rejects_dollar_injection() {
        assert!(validate_flake_ref("github:org/${evil}").is_err());
    }

    #[test]
    fn rejects_semicolon_injection() {
        assert!(validate_flake_ref("github:org/app;rm -rf /").is_err());
    }

    #[test]
    fn rejects_backtick() {
        assert!(validate_flake_ref("github:org/`whoami`").is_err());
    }

    #[test]
    fn rejects_space() {
        assert!(validate_flake_ref("github:org/app with spaces").is_err());
    }

    #[test]
    fn rejects_parentheses() {
        assert!(validate_flake_ref("github:org/(evil)").is_err());
    }

    // -- validate_attribute --

    #[test]
    fn valid_simple_attribute() {
        assert!(validate_attribute("default").is_ok());
    }

    #[test]
    fn valid_dotted_attribute() {
        assert!(validate_attribute("packages.x86_64-linux.default").is_ok());
    }

    #[test]
    fn valid_underscore_start() {
        assert!(validate_attribute("_private.output").is_ok());
    }

    #[test]
    fn rejects_empty_attribute() {
        assert!(validate_attribute("").is_err());
    }

    #[test]
    fn rejects_empty_segment() {
        assert!(validate_attribute("packages..default").is_err());
    }

    #[test]
    fn rejects_semicolon_in_attribute() {
        assert!(validate_attribute("x; builtins.abort").is_err());
    }

    #[test]
    fn rejects_quote_in_attribute() {
        assert!(validate_attribute(r#"x"evil"#).is_err());
    }

    #[test]
    fn rejects_digit_start_segment() {
        assert!(validate_attribute("packages.123bad").is_err());
    }

    // -- validate_revision --

    #[test]
    fn valid_short_sha() {
        assert!(validate_revision("a1b2c3").is_ok());
    }

    #[test]
    fn valid_full_sha() {
        assert!(validate_revision("a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2").is_ok());
    }

    #[test]
    fn rejects_too_short() {
        assert!(validate_revision("abc").is_err());
    }

    #[test]
    fn rejects_too_long() {
        assert!(validate_revision(&"a".repeat(41)).is_err());
    }

    #[test]
    fn rejects_non_hex() {
        assert!(validate_revision("g1h2i3j4k5").is_err());
    }

    #[test]
    fn rejects_injection_in_revision() {
        assert!(validate_revision(r#"abc123"; rm -rf /"#).is_err());
    }

    // -- validate_eval_request --

    #[test]
    fn valid_full_request() {
        let req = EvalRequest {
            flake_ref: "github:org/app".into(),
            attribute: "packages.x86_64-linux.default".into(),
            revision: Some("a1b2c3d4e5f6".into()),
            binary_cache: None,
        };
        assert!(validate_eval_request(&req).is_ok());
    }

    #[test]
    fn valid_request_without_revision() {
        let req = EvalRequest {
            flake_ref: "github:org/app".into(),
            attribute: "packages.x86_64-linux.default".into(),
            revision: None,
            binary_cache: None,
        };
        assert!(validate_eval_request(&req).is_ok());
    }

    #[test]
    fn invalid_flake_ref_in_request() {
        let req = EvalRequest {
            flake_ref: r#"github:org/app" ++ evil"#.into(),
            attribute: "default".into(),
            revision: None,
            binary_cache: None,
        };
        assert!(validate_eval_request(&req).is_err());
    }

    #[test]
    fn invalid_attribute_in_request() {
        let req = EvalRequest {
            flake_ref: "github:org/app".into(),
            attribute: "x; builtins.abort".into(),
            revision: None,
            binary_cache: None,
        };
        assert!(validate_eval_request(&req).is_err());
    }

    #[test]
    fn invalid_revision_in_request() {
        let req = EvalRequest {
            flake_ref: "github:org/app".into(),
            attribute: "default".into(),
            revision: Some("not-hex!!".into()),
            binary_cache: None,
        };
        assert!(validate_eval_request(&req).is_err());
    }
}
