//! FIX 3: the runner's private `TOOLU_RUNNER_*` env namespace (incl. the
//! admin-scoped re-mint bearer `TOOLU_RUNNER_TOKEN`) must never be folded into
//! a job/step child env. Unit-tests the pure predicate and the fold filter the
//! step/hook/composite/node sites now run.

use std::collections::HashMap;

use execution::execution::context::is_runner_private_env_key;

#[test]
fn predicate_flags_the_runner_private_namespace() {
  assert!(is_runner_private_env_key("TOOLU_RUNNER_TOKEN"));
  assert!(is_runner_private_env_key("toolu_runner_token")); // case-insensitive
  assert!(is_runner_private_env_key("TOOLU_RUNNER_CLIENT_ID"));
  assert!(is_runner_private_env_key("Toolu_Runner_Home"));
}

#[test]
fn predicate_passes_ordinary_env_keys() {
  assert!(!is_runner_private_env_key("PATH"));
  assert!(!is_runner_private_env_key("HOME"));
  assert!(!is_runner_private_env_key("LANG"));
  assert!(!is_runner_private_env_key("GITHUB_TOKEN"));
  assert!(!is_runner_private_env_key("TOOLU")); // shorter than the prefix
  assert!(!is_runner_private_env_key("TOOLU_RUNNER")); // no trailing `_`
  assert!(!is_runner_private_env_key("")); // empty
}

#[test]
fn fold_filter_drops_the_token_but_keeps_path_home_lang() {
  // Mirror the fold the sites now run: a process-env iterator filtered through
  // `is_runner_private_env_key`. Use a SYNTHETIC map (never mutate real
  // std::env — racy) to prove the predicate-driven filter's outcome.
  let synthetic = [
    ("TOOLU_RUNNER_TOKEN", "ghs_adminsecret"),
    ("TOOLU_RUNNER_CLIENT_ID", "Iv1.abc"),
    ("PATH", "/usr/bin:/bin"),
    ("HOME", "/home/runner"),
    ("LANG", "en_US.UTF-8"),
  ];
  let folded: HashMap<&str, &str> = synthetic
    .into_iter()
    .filter(|(k, _)| !is_runner_private_env_key(k))
    .collect();

  assert!(
    !folded.contains_key("TOOLU_RUNNER_TOKEN"),
    "the admin re-mint bearer must be stripped from the child env"
  );
  assert!(!folded.contains_key("TOOLU_RUNNER_CLIENT_ID"));
  assert_eq!(folded.get("PATH"), Some(&"/usr/bin:/bin"));
  assert_eq!(folded.get("HOME"), Some(&"/home/runner"));
  assert_eq!(folded.get("LANG"), Some(&"en_US.UTF-8"));
}
