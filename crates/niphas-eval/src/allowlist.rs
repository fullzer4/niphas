use crate::error::AppError;

/// Validate a flake reference against the allowlist.
///
/// The allowlist uses simple glob patterns:
/// - `*` matches any sequence of characters (including `/` and empty string)
///
/// Matching is case-sensitive (Nix flake refs are case-sensitive).
///
/// Note: `github:*` would match `github:` (empty path after scheme), but this
/// is harmless because `validate_eval_request()` in `eval.rs` already rejects
/// refs without a valid `scheme:path` format with non-empty path.
///
/// Examples:
/// - `github:myorg/*` matches `github:myorg/myapp` and `github:myorg/myapp?dir=sub`
/// - `github:nixos/nixpkgs` matches exactly
/// - `github:*/*` matches any two-segment github flake
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

/// Simple glob matching for flake refs (zero allocation).
fn matches_glob(input: &str, pattern: &str) -> bool {
    let (ib, pb) = (input.as_bytes(), pattern.as_bytes());
    let (mut i, mut p) = (0, 0);
    let (mut star_p, mut star_i) = (usize::MAX, 0);

    while i < ib.len() {
        if p < pb.len() && pb[p] == b'*' {
            star_p = p;
            star_i = i;
            p += 1;
        } else if p < pb.len() && pb[p] == ib[i] {
            i += 1;
            p += 1;
        } else if star_p != usize::MAX {
            star_i += 1;
            i = star_i;
            p = star_p + 1;
        } else {
            return false;
        }
    }

    while p < pb.len() && pb[p] == b'*' {
        p += 1;
    }

    p == pb.len()
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

    #[test]
    fn test_wildcard_matches_empty_suffix() {
        // `*` matches empty string, so `github:*` matches `github:`
        // This is harmless — validate_eval_request() rejects bare `github:`.
        assert!(matches_glob("github:", "github:*"));
    }

    #[test]
    fn test_case_sensitivity() {
        // Nix flake refs are case-sensitive
        assert!(!matches_glob("GitHub:myorg/app", "github:myorg/*"));
        assert!(!matches_glob("github:MyOrg/app", "github:myorg/*"));
        assert!(matches_glob("github:myorg/app", "github:myorg/*"));
    }

    #[test]
    fn test_wildcard_with_query_params() {
        assert!(matches_glob("github:myorg/myapp?dir=sub", "github:myorg/*"));
    }

    #[test]
    fn test_path_scheme() {
        assert!(matches_glob("path:/local/flake", "path:*"));
        assert!(!matches_glob("path:/local/flake", "github:*"));
    }
}
