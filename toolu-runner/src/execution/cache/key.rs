//! Cache key design and fallback ladder resolution.
//!
//! Key format: `{version}:{repo}:{branch}:{content_hash}`
//! Fallback: exact key → branch prefix → default branch.

/// Build a cache key from components.
pub fn build_cache_key(version: &str, repo: &str, branch: &str, content_hash: &str) -> String {
  format!("{version}:{repo}:{branch}:{content_hash}")
}

/// Generate fallback keys for cache lookup (exact → branch → default).
///
/// Returns keys in priority order: most specific first.
pub fn fallback_keys(
  version: &str,
  repo: &str,
  branch: &str,
  content_hash: &str,
  default_branch: &str,
) -> Vec<String> {
  let mut keys = vec![
    // Exact match.
    build_cache_key(version, repo, branch, content_hash),
    // Branch prefix (any hash).
    format!("{version}:{repo}:{branch}:"),
  ];
  // Default branch fallback (only if not already the default).
  if branch != default_branch {
    keys.push(format!("{version}:{repo}:{default_branch}:"));
  }
  keys
}

/// Check if a stored key matches a lookup key (exact or prefix).
pub fn key_matches(stored_key: &str, lookup_key: &str) -> bool {
  if lookup_key.ends_with(':') {
    stored_key.starts_with(lookup_key)
  } else {
    stored_key == lookup_key
  }
}
