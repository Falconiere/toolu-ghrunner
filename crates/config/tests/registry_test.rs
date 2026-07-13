//! Integration tests for `config::registry` (zero-arg-register AC-9).
//!
//! Real data only: registrations are real `config.toml` files (the
//! `RuntimeConfig` TOML shape `save_config` writes) inside real `tempfile`
//! homes. The `runner_home` env behavior is proven by re-running this test
//! binary as a subprocess with `TOOLU_RUNNER_HOME` set / removed —
//! in-process `std::env::set_var` is unsafe (edition 2024) and racy across
//! parallel test threads.

use std::path::{Path, PathBuf};

use config::registry::{self, RegistrationEntry};
use tempfile::TempDir;

/// The TOML body `save_config` persists — what a real registration holds.
const CONFIG_TOML: &str = concat!(
  "jit_config = \"eyIucnVubmVyIjoiZTMwPSJ9\"\n",
  "work_dir = \"/Users/runner/.toolu-runner/_work\"\n",
  "data_dir = \"/Users/runner/.toolu-runner\"\n",
  "protocol_version = \"v2\"\n",
);

/// Create `<home>/runners/<owner>/<repo>/config.toml` and return its path.
fn add_registration(home: &Path, owner: &str, repo: &str) -> Result<PathBuf, std::io::Error> {
  let dir = home.join("runners").join(owner).join(repo);
  std::fs::create_dir_all(&dir)?;
  let config_path = dir.join("config.toml");
  std::fs::write(&config_path, CONFIG_TOML)?;
  Ok(config_path)
}

/// Create the legacy single-slot `<home>/config.toml` and return its path.
fn add_legacy_registration(home: &Path) -> Result<PathBuf, std::io::Error> {
  let config_path = home.join("config.toml");
  std::fs::write(&config_path, CONFIG_TOML)?;
  Ok(config_path)
}

// ── runner_home: $TOOLU_RUNNER_HOME override vs ~/.toolu-runner default ─

/// Subprocess helper: prints `runner_home()` when driven by the two
/// `runner_home_*` tests below; a no-op pass in a normal suite run.
#[test]
fn helper_print_runner_home() {
  if std::env::var_os("REGISTRY_TEST_PRINT_HOME").is_some() {
    println!("runner_home={}", registry::runner_home().display());
  }
}

/// Re-run this test binary filtered to `helper_print_runner_home` with
/// `TOOLU_RUNNER_HOME` set to `env_home` (or removed) and return its stdout.
fn runner_home_via_subprocess(env_home: Option<&Path>) -> Result<String, std::io::Error> {
  let exe = std::env::current_exe()?;
  let mut cmd = std::process::Command::new(exe);
  cmd.args(["helper_print_runner_home", "--exact", "--nocapture"]);
  cmd.env("REGISTRY_TEST_PRINT_HOME", "1");
  match env_home {
    Some(path) => cmd.env("TOOLU_RUNNER_HOME", path),
    None => cmd.env_remove("TOOLU_RUNNER_HOME"),
  };
  let out = cmd.output()?;
  assert!(
    out.status.success(),
    "helper run failed: {}",
    String::from_utf8_lossy(&out.stderr)
  );
  Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
fn runner_home_prefers_env_override() {
  let home = TempDir::new().unwrap();
  let stdout = runner_home_via_subprocess(Some(home.path())).unwrap();
  assert!(
    stdout.contains(&format!("runner_home={}", home.path().display())),
    "TOOLU_RUNNER_HOME must win; helper printed: {stdout}"
  );
}

#[test]
fn runner_home_defaults_to_dot_toolu_runner() {
  let stdout = runner_home_via_subprocess(None).unwrap();
  let expected = shared::paths::expand_tilde(Path::new("~/.toolu-runner"));
  assert!(
    stdout.contains(&format!("runner_home={}", expected.display())),
    "without TOOLU_RUNNER_HOME the default must be ~/.toolu-runner; helper printed: {stdout}"
  );
}

// ── runner_dir: layout + path-component rejection ───────────────────

#[test]
fn runner_dir_builds_home_runners_owner_repo() {
  let home = Path::new("/var/lib/toolu-runner");
  let dir = registry::runner_dir(home, "octocat", "hello-world").unwrap();
  assert_eq!(
    dir,
    PathBuf::from("/var/lib/toolu-runner/runners/octocat/hello-world")
  );
}

#[test]
fn runner_dir_rejects_non_component_names() {
  let home = Path::new("/var/lib/toolu-runner");
  let bad = [
    ("../escape", "repo"),
    ("owner", ".."),
    ("owner/nested", "repo"),
    ("owner", "re/po"),
    ("owner", "re\\po"),
    ("", "repo"),
    ("owner", ""),
    (".", "repo"),
    ("owner", "."),
  ];
  for (owner, repo) in bad {
    let err = registry::runner_dir(home, owner, repo).unwrap_err();
    assert!(
      err.to_string().contains("must be a single path component"),
      "({owner:?}, {repo:?}) must be rejected; got: {err}"
    );
  }
}

// ── list_registrations: discovery, ordering, empty home ─────────────

#[test]
fn list_registrations_empty_home_yields_empty_vec() {
  let home = TempDir::new().unwrap();
  assert_eq!(registry::list_registrations(home.path()), Vec::new());
}

#[test]
fn list_registrations_missing_home_yields_empty_vec() {
  let home = TempDir::new().unwrap();
  let gone = home.path().join("never-created");
  assert_eq!(registry::list_registrations(&gone), Vec::new());
}

#[test]
fn list_registrations_sorts_by_owner_repo_with_legacy_last() {
  let home = TempDir::new().unwrap();
  let zebra = add_registration(home.path(), "zebra", "stripes").unwrap();
  let apple = add_registration(home.path(), "apple", "pie").unwrap();
  let apple_zz = add_registration(home.path(), "apple", "zz-top").unwrap();
  let legacy = add_legacy_registration(home.path()).unwrap();
  // A repo dir without a config.toml is not a registration.
  std::fs::create_dir_all(home.path().join("runners").join("ghost").join("empty")).unwrap();
  // A stray file directly under runners/ is skipped, not an error.
  std::fs::write(home.path().join("runners").join("stray.txt"), "junk").unwrap();

  let entries = registry::list_registrations(home.path());
  assert_eq!(
    entries,
    vec![
      RegistrationEntry {
        config_path: apple,
        owner_repo: Some("apple/pie".to_owned()),
      },
      RegistrationEntry {
        config_path: apple_zz,
        owner_repo: Some("apple/zz-top".to_owned()),
      },
      RegistrationEntry {
        config_path: zebra,
        owner_repo: Some("zebra/stripes".to_owned()),
      },
      RegistrationEntry {
        config_path: legacy,
        owner_repo: None,
      },
    ]
  );
}

// ── resolve_config_path: the five precedence branches (AC-9) ────────

#[test]
fn resolve_flag_beats_everything() {
  let home = TempDir::new().unwrap();
  add_registration(home.path(), "octocat", "hello-world").unwrap();
  let flag = PathBuf::from("/somewhere/else/config.toml");
  let resolved = registry::resolve_config_path(
    Some(flag.clone()),
    home.path(),
    Some(("octocat", "hello-world")),
  )
  .unwrap();
  assert_eq!(
    resolved, flag,
    "an explicit --config must be returned as-is"
  );
}

#[test]
fn resolve_inferred_beats_other_registrations_when_its_config_exists() {
  let home = TempDir::new().unwrap();
  add_registration(home.path(), "apple", "pie").unwrap();
  let matched = add_registration(home.path(), "octocat", "hello-world").unwrap();
  let resolved =
    registry::resolve_config_path(None, home.path(), Some(("octocat", "hello-world"))).unwrap();
  assert_eq!(resolved, matched);
}

#[test]
fn resolve_ignores_inference_without_a_config_and_takes_the_sole_registration() {
  let home = TempDir::new().unwrap();
  let only = add_registration(home.path(), "apple", "pie").unwrap();
  let resolved =
    registry::resolve_config_path(None, home.path(), Some(("octocat", "unregistered"))).unwrap();
  assert_eq!(
    resolved, only,
    "inference without a persisted config must fall through to the sole registration"
  );
}

#[test]
fn resolve_sole_legacy_registration_wins_without_inference() {
  let home = TempDir::new().unwrap();
  let legacy = add_legacy_registration(home.path()).unwrap();
  let resolved = registry::resolve_config_path(None, home.path(), None).unwrap();
  assert_eq!(resolved, legacy);
}

#[test]
fn resolve_zero_registrations_errors_naming_register() {
  let home = TempDir::new().unwrap();
  let err = registry::resolve_config_path(None, home.path(), None).unwrap_err();
  assert!(
    err.to_string().contains("toolu-runner register"),
    "the no-registration error must name `toolu-runner register`; got: {err}"
  );
}

#[test]
fn resolve_ambiguous_registrations_error_lists_each_candidate() {
  let home = TempDir::new().unwrap();
  add_registration(home.path(), "apple", "pie").unwrap();
  add_registration(home.path(), "zebra", "stripes").unwrap();
  add_legacy_registration(home.path()).unwrap();
  let err = registry::resolve_config_path(None, home.path(), None).unwrap_err();
  let msg = err.to_string();
  for candidate in ["apple/pie", "zebra/stripes", "legacy", "--config"] {
    assert!(
      msg.contains(candidate),
      "ambiguity error must mention {candidate:?}; got: {msg}"
    );
  }
}
