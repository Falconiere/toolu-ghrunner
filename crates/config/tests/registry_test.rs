//! Integration tests for `config::registry` (zero-arg-register AC-9).
//!
//! Real data only: registrations are real `config.toml` files (the
//! `RuntimeConfig` TOML shape `save_config` writes) inside real `tempfile`
//! homes. The `runner_home` env behavior is proven by re-running this test
//! binary as a subprocess with `TOOLU_RUNNER_HOME` set / removed —
//! in-process `std::env::set_var` is unsafe (edition 2024) and racy across
//! parallel test threads.

use std::path::{Path, PathBuf};

use config::registry::{self, RegistrationEntry, RegistrationScope};
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

/// Create `<home>/orgs/<org>/config.toml` and return its path.
fn add_org_registration(home: &Path, org: &str) -> Result<PathBuf, std::io::Error> {
  let dir = home.join("orgs").join(org);
  std::fs::create_dir_all(&dir)?;
  let config_path = dir.join("config.toml");
  std::fs::write(&config_path, CONFIG_TOML)?;
  Ok(config_path)
}

/// The scope of a per-repo registration.
fn repo(owner: &str, name: &str) -> RegistrationScope {
  RegistrationScope::Repo {
    owner: owner.to_owned(),
    repo: name.to_owned(),
  }
}

/// Create the legacy single-slot `<home>/config.toml` and return its path.
fn add_legacy_registration(home: &Path) -> Result<PathBuf, std::io::Error> {
  let config_path = home.join("config.toml");
  std::fs::write(&config_path, CONFIG_TOML)?;
  Ok(config_path)
}

// ── runner_home: $TOOLU_RUNNER_HOME override vs ~/.toolu-runner default ─
//
// WHY subprocess re-exec: `runner_home()` reads the process environment,
// and mutating it in-process is off the table — under edition 2024
// `std::env::set_var` is `unsafe` and the workspace denies `unsafe_code`
// (it would also race the parallel test threads). So each case re-runs
// THIS test binary filtered to `helper_print_runner_home` with the env
// prepared on the child. `--exact` and `--nocapture` are stable libtest
// flags: run only that exact test name, and let its stdout through for
// the parent to assert on.

/// Subprocess helper: prints `runner_home()` when driven by the
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
  runner_home_subprocess(|cmd| {
    match env_home {
      Some(path) => cmd.env("TOOLU_RUNNER_HOME", path),
      None => cmd.env_remove("TOOLU_RUNNER_HOME"),
    };
  })
}

/// Re-run this test binary filtered to `helper_print_runner_home`,
/// letting `prepare` set the child's env; return its stdout.
fn runner_home_subprocess(
  prepare: impl FnOnce(&mut std::process::Command),
) -> Result<String, std::io::Error> {
  let exe = std::env::current_exe()?;
  let mut cmd = std::process::Command::new(exe);
  cmd.args([
    "registry_test::helper_print_runner_home",
    "--exact",
    "--nocapture",
  ]);
  cmd.env("REGISTRY_TEST_PRINT_HOME", "1");
  prepare(&mut cmd);
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

#[test]
fn runner_home_expands_tilde_in_env_override() {
  // HOME is pinned on the child so `~` expands into a hermetic tempdir,
  // not the developer's real home.
  let fake_home = TempDir::new().unwrap();
  let stdout = runner_home_subprocess(|cmd| {
    cmd.env("TOOLU_RUNNER_HOME", "~/custom-runner-home");
    cmd.env("HOME", fake_home.path());
  })
  .unwrap();
  let expected = fake_home.path().join("custom-runner-home");
  assert!(
    stdout.contains(&format!("runner_home={}", expected.display())),
    "a `~/x` TOOLU_RUNNER_HOME must tilde-expand; helper printed: {stdout}"
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
    ("own\0er", "repo"),
    ("owner", "re\0po"),
  ];
  for (owner, repo) in bad {
    let err = registry::runner_dir(home, owner, repo).unwrap_err();
    assert!(
      err.to_string().contains("must be a single path component"),
      "({owner:?}, {repo:?}) must be rejected; got: {err}"
    );
  }
}

// ── org_dir: layout + path-component rejection (#208) ───────────────

#[test]
fn org_dir_builds_home_orgs_org() {
  let home = Path::new("/var/lib/toolu-runner");
  assert_eq!(
    registry::org_dir(home, "acme").unwrap(),
    PathBuf::from("/var/lib/toolu-runner/orgs/acme")
  );
}

#[test]
fn org_dir_rejects_non_component_names() {
  let home = Path::new("/var/lib/toolu-runner");
  for org in ["", ".", "..", "a/b", "a\\b", "ac\0me"] {
    let err = registry::org_dir(home, org).unwrap_err();
    let msg = err.to_string();
    assert!(
      msg.contains("must be a single path component") && msg.contains("invalid org"),
      "{org:?} must be rejected naming the org; got: {msg}"
    );
  }
}

/// AC-10: two org registrations and two repo registrations of one owner
/// coexist with the legacy slot; each is listed once, repos first (by
/// owner/repo), then orgs (by name), legacy last.
#[test]
fn list_registrations_lists_orgs_between_repos_and_legacy() {
  let home = TempDir::new().unwrap();
  let beta = add_org_registration(home.path(), "beta").unwrap();
  let r2 = add_registration(home.path(), "acme", "r2").unwrap();
  let acme = add_org_registration(home.path(), "acme").unwrap();
  let r1 = add_registration(home.path(), "acme", "r1").unwrap();
  let legacy = add_legacy_registration(home.path()).unwrap();
  // Neither a stray file under orgs/ nor an org dir without a config.toml
  // is a registration.
  std::fs::write(home.path().join("orgs").join("stray.txt"), "junk").unwrap();
  std::fs::create_dir_all(home.path().join("orgs").join("ghost")).unwrap();

  let entries = registry::list_registrations(home.path()).unwrap();
  assert_eq!(
    entries,
    vec![
      RegistrationEntry {
        config_path: r1,
        scope: repo("acme", "r1"),
      },
      RegistrationEntry {
        config_path: r2,
        scope: repo("acme", "r2"),
      },
      RegistrationEntry {
        config_path: acme,
        scope: RegistrationScope::Org("acme".to_owned()),
      },
      RegistrationEntry {
        config_path: beta,
        scope: RegistrationScope::Org("beta".to_owned()),
      },
      RegistrationEntry {
        config_path: legacy,
        scope: RegistrationScope::Legacy,
      },
    ]
  );
  let labels: Vec<String> = entries.iter().map(|e| e.scope.label()).collect();
  assert_eq!(
    labels,
    ["acme/r1", "acme/r2", "acme (org)", "beta (org)", "legacy"]
  );
}

/// Create `<home>/toolu/<org>/config.toml` and return its path.
fn add_toolu_registration(home: &Path, org: &str) -> Result<PathBuf, std::io::Error> {
  let dir = home.join("toolu").join(org);
  std::fs::create_dir_all(&dir)?;
  let config_path = dir.join("config.toml");
  std::fs::write(&config_path, CONFIG_TOML)?;
  Ok(config_path)
}

/// #206 AC-8: a toolu.sh machine registration (`toolu/<org>/`) is listed
/// after the organizations and before the legacy slot; a `toolu` repo owner
/// lives under `runners/` and never collides with it.
#[test]
fn list_registrations_lists_toolu_machines_between_orgs_and_legacy() {
  let home = TempDir::new().unwrap();
  let toolu = add_toolu_registration(home.path(), "org_123").unwrap();
  let owner_named_toolu = add_registration(home.path(), "toolu", "web").unwrap();
  let org = add_org_registration(home.path(), "acme").unwrap();
  let legacy = add_legacy_registration(home.path()).unwrap();
  std::fs::write(home.path().join("toolu").join("stray.txt"), "junk").unwrap();
  std::fs::create_dir_all(home.path().join("toolu").join("ghost")).unwrap();

  let entries = registry::list_registrations(home.path()).unwrap();
  let paths: Vec<&PathBuf> = entries.iter().map(|e| &e.config_path).collect();
  assert_eq!(paths, [&owner_named_toolu, &org, &toolu, &legacy]);
  assert_eq!(
    entries.get(2).map(|e| &e.scope),
    Some(&RegistrationScope::Toolu("org_123".to_owned()))
  );
  let labels: Vec<String> = entries.iter().map(|e| e.scope.label()).collect();
  assert_eq!(
    labels,
    ["toolu/web", "acme (org)", "org_123 (toolu.sh)", "legacy"]
  );
}

#[test]
fn toolu_dir_builds_home_toolu_org_and_rejects_non_components() {
  let home = Path::new("/var/lib/toolu-runner");
  assert_eq!(
    registry::toolu_dir(home, "org_123").unwrap(),
    PathBuf::from("/var/lib/toolu-runner/toolu/org_123")
  );
  for org in ["", ".", "..", "a/b", "a\\b", "or\0g"] {
    let err = registry::toolu_dir(home, org).unwrap_err();
    assert!(
      err.to_string().contains("must be a single path component"),
      "{org:?} must be rejected; got: {err}"
    );
  }
}

#[test]
fn resolve_sole_toolu_registration_wins() {
  let home = TempDir::new().unwrap();
  let toolu = add_toolu_registration(home.path(), "org_123").unwrap();
  assert_eq!(
    registry::resolve_config_path(None, home.path(), None).unwrap(),
    toolu
  );
}

#[test]
fn resolve_sole_org_registration_wins() {
  let home = TempDir::new().unwrap();
  let org = add_org_registration(home.path(), "acme").unwrap();
  assert_eq!(
    registry::resolve_config_path(None, home.path(), None).unwrap(),
    org
  );
}

// ── list_registrations: discovery, ordering, empty home ─────────────

#[test]
fn list_registrations_empty_home_yields_empty_vec() {
  let home = TempDir::new().unwrap();
  assert_eq!(
    registry::list_registrations(home.path()).unwrap(),
    Vec::new()
  );
}

#[test]
fn list_registrations_missing_home_yields_empty_vec() {
  let home = TempDir::new().unwrap();
  let gone = home.path().join("never-created");
  assert_eq!(registry::list_registrations(&gone).unwrap(), Vec::new());
}

/// A stray file directly under `runners/` (or under an owner dir) is not
/// a registration — the explicit `is_dir()` filter skips it silently.
#[test]
fn list_registrations_ignores_stray_files_at_both_levels() {
  let home = TempDir::new().unwrap();
  let real = add_registration(home.path(), "octocat", "hello-world").unwrap();
  std::fs::write(home.path().join("runners").join("stray.txt"), "junk").unwrap();
  std::fs::write(
    home.path().join("runners").join("octocat").join("notes.md"),
    "junk",
  )
  .unwrap();

  let entries = registry::list_registrations(home.path()).unwrap();
  assert_eq!(
    entries,
    vec![RegistrationEntry {
      config_path: real,
      scope: repo("octocat", "hello-world"),
    }],
    "stray files must be skipped, never error, never register"
  );
}

/// An EXISTING but unreadable `runners/` dir is an error naming the path
/// — silently reporting "no registrations" would misdirect the user to
/// `register` when the real problem is permissions. Unix-only: dir modes
/// are not enforceable this way elsewhere.
#[cfg(unix)]
#[test]
fn list_registrations_unreadable_runners_dir_is_an_error() {
  use std::os::unix::fs::PermissionsExt;
  let home = TempDir::new().unwrap();
  add_registration(home.path(), "octocat", "hello-world").unwrap();
  let runners = home.path().join("runners");
  std::fs::set_permissions(&runners, std::fs::Permissions::from_mode(0o000)).unwrap();
  // A privileged user (root CI containers) ignores dir modes — skip there.
  if std::fs::read_dir(&runners).is_ok() {
    std::fs::set_permissions(&runners, std::fs::Permissions::from_mode(0o755)).unwrap();
    eprintln!("skipping: this user can read a 000 dir (running privileged)");
    return;
  }

  let result = registry::list_registrations(home.path());

  // Restore before asserting so the tempdir cleans up even on failure.
  std::fs::set_permissions(&runners, std::fs::Permissions::from_mode(0o755)).unwrap();
  let err = result.unwrap_err();
  let msg = err.to_string();
  assert!(
    msg.contains(&runners.display().to_string()),
    "error must name the unreadable path; got: {msg}"
  );
  assert!(
    msg.contains("cannot read registrations dir"),
    "error must say the dir could not be read (io error appended); got: {msg}"
  );
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

  let entries = registry::list_registrations(home.path()).unwrap();
  assert_eq!(
    entries,
    vec![
      RegistrationEntry {
        config_path: apple,
        scope: repo("apple", "pie"),
      },
      RegistrationEntry {
        config_path: apple_zz,
        scope: repo("apple", "zz-top"),
      },
      RegistrationEntry {
        config_path: zebra,
        scope: repo("zebra", "stripes"),
      },
      RegistrationEntry {
        config_path: legacy,
        scope: RegistrationScope::Legacy,
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
  add_org_registration(home.path(), "acme").unwrap();
  add_legacy_registration(home.path()).unwrap();
  let err = registry::resolve_config_path(None, home.path(), None).unwrap_err();
  let msg = err.to_string();
  for candidate in [
    "apple/pie",
    "zebra/stripes",
    "acme (org)",
    "legacy",
    "--config",
  ] {
    assert!(
      msg.contains(candidate),
      "ambiguity error must mention {candidate:?}; got: {msg}"
    );
  }
}
