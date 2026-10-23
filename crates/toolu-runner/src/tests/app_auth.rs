//! #201: the installation-token cache's reuse rule — a cached token serves
//! only its own installation, and only before its deadline.

use std::sync::Mutex;

use super::{CachedToken, InstallationTokens};

fn cache(installation_id: i64, reuse_until: i64) -> InstallationTokens {
  InstallationTokens {
    cached: Mutex::new(Some(CachedToken {
      installation_id,
      token: "ghs_EXAMPLE-cached".to_owned(),
      reuse_until,
    })),
  }
}

#[test]
fn a_cached_token_is_reused_for_its_installation_before_the_deadline() {
  let tokens = cache(7, 1_000);
  assert_eq!(
    tokens.reusable(7, 999).as_deref(),
    Some("ghs_EXAMPLE-cached")
  );
}

#[test]
fn a_cached_token_is_not_reused_at_or_after_the_deadline() {
  let tokens = cache(7, 1_000);
  assert_eq!(tokens.reusable(7, 1_000), None);
  assert_eq!(tokens.reusable(7, 5_000), None);
}

#[test]
fn a_cached_token_never_serves_another_installation() {
  let tokens = cache(7, 1_000);
  assert_eq!(tokens.reusable(8, 1), None);
}

#[test]
fn an_empty_cache_reuses_nothing() {
  assert_eq!(InstallationTokens::default().reusable(7, 1), None);
}
