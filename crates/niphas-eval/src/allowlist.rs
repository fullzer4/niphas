use crate::error::AppError;

/// Validate a flake reference against the allowlist.
///
/// The allowlist uses simple glob patterns:
/// - `*` matches any sequence of non-`/` characters
/// - `**` is not supported (flake refs don't have deep paths)
///
/// Examples:
/// - `github:myorg/*` matches `github:myorg/myapp`
/// - `github:nixos/nixpkgs` matches exactly
/// - `github:*/*` matches any github flake
pub fn validate_flake_ref(flake_ref: &str, allowlist: &[String]) -> Result<(), AppError> {
    if allowlist.is_empty() {
        // Empty allowlist = deny all
        return Err(AppError::FlakeNotAllowed(format!(
            "{flake_ref}: no flake origins are allowed (empty allowlist)"
        )));
    }

    for pattern in allowlist {
        if matches_glob(flake_ref, pattern) {
            return Ok(());
        }
    }

    Err(AppError::FlakeNotAllowed(format!(
        "{flake_ref} not in allowlist"
    )))
}

/// Simple glob matching for flake refs.
fn matches_glob(input: &str, pattern: &str) -> bool {
    let mut input_chars = input.chars().peekable();
    let mut pattern_chars = pattern.chars().peekable();

    loop {
        match (pattern_chars.peek(), input_chars.peek()) {
            (Some(&'*'), _) => {
                pattern_chars.next();
                // '*' matches everything to end if no more pattern
                if pattern_chars.peek().is_none() {
                    return true;
                }
                // Try matching rest of pattern at each position
                let remaining_pattern: String = pattern_chars.collect();
                let remaining_input: String = input_chars.collect();
                for i in 0..=remaining_input.len() {
                    if matches_glob(&remaining_input[i..], &remaining_pattern) {
                        return true;
                    }
                }
                return false;
            }
            (Some(&pc), Some(&ic)) => {
                if pc == ic {
                    pattern_chars.next();
                    input_chars.next();
                } else {
                    return false;
                }
            }
            (None, None) => return true,
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        assert!(matches_glob("github:nixos/nixpkgs", "github:nixos/nixpkgs"));
    }

    #[test]
    fn test_wildcard_suffix() {
        assert!(matches_glob("github:myorg/myapp", "github:myorg/*"));
        assert!(matches_glob("github:myorg/lib", "github:myorg/*"));
        assert!(!matches_glob("github:other/repo", "github:myorg/*"));
    }

    #[test]
    fn test_double_wildcard() {
        assert!(matches_glob("github:myorg/myapp", "github:*/*"));
        assert!(matches_glob("github:nixos/nixpkgs", "github:*/*"));
        assert!(!matches_glob("path:/local/flake", "github:*/*"));
    }

    #[test]
    fn test_no_match() {
        assert!(!matches_glob("github:evil/repo", "github:myorg/*"));
    }

    #[test]
    fn test_validate_allowlist() {
        let allowlist = vec![
            "github:myorg/*".to_string(),
            "github:nixos/nixpkgs".to_string(),
        ];

        assert!(validate_flake_ref("github:myorg/app", &allowlist).is_ok());
        assert!(validate_flake_ref("github:nixos/nixpkgs", &allowlist).is_ok());
        assert!(validate_flake_ref("github:evil/repo", &allowlist).is_err());
    }

    #[test]
    fn test_empty_allowlist_denies_all() {
        assert!(validate_flake_ref("github:myorg/app", &[]).is_err());
    }
}
