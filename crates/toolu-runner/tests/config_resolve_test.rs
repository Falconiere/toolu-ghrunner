//! Config-path resolution e2e (spec 2026-07-12-zero-arg-register-design,
//! AC-7 / AC-9) plus `remove`'s per-repo file semantics (plan OQ-3).
//!
//! Every test shells out the debug binary (`CARGO_BIN_EXE_toolu-runner`)
//! with `TOOLU_RUNNER_HOME` and `HOME` pinned to a fresh tempdir home and
//! the child's cwd pinned explicitly — a plain tempdir when inference must
//! not fire, a real `git init` repo when it must. Registrations are real
//! `config.toml` files written through `config::config::save_config` (the
//! exact writer `register` uses). No network: `status` and `remove` read
//! and delete local state only.

use std::path::{Path, PathBuf};

use config::config::{
  CacheSection, RunnerRegistrationConfig, RuntimeConfig, ServicesSection, ShadowSection,
  WorkspaceSection, save_config,
};
use shared::RunnerError;

/// Base `toolu-runner` invocation: `TOOLU_RUNNER_HOME` and `HOME` pinned
/// to the test home (hermetic registry, no gitconfig URL rewrites in the
/// child's git calls), cwd pinned to `cwd`, stdio piped.
fn runner_cmd(home: &Path, cwd: &Path) -> std::process::Command {
  let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_toolu-runner"));
  cmd
    .env("TOOLU_RUNNER_HOME", home)
    .env("HOME", home)
    .current_dir(cwd)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped());
  cmd
}

/// Write a real registration `config.toml` into `dir` via `save_config`,
/// registered against `https://github.com/<owner>/<repo>` with `data_dir`
/// = `dir` itself (the per-repo contract `register` persists). `?` (not
/// `expect`) keeps this non-`#[test]` helper clippy-clean.
fn write_registration(
  dir: &Path,
  runner_name: &str,
  owner: &str,
  repo: &str,
) -> Result<PathBuf, RunnerError> {
  let config_path = dir.join("config.toml");
  let config = RunnerRegistrationConfig {
    runner_url: format!("https://github.com/{owner}/{repo}"),
    runner_name: runner_name.to_owned(),
    runner_id: 461,
    auth_token: "fixture-client-id".to_owned(),
    labels: vec!["self-hosted".to_owned()],
    runner_group: "Default".to_owned(),
    runtime: RuntimeConfig {
      jit_config: "fixture-jit-blob".to_owned(),
      work_dir: "~/.toolu-runner/_work".to_owned(),
      data_dir: dir.to_string_lossy().into_owned(),
      protocol_version: "v2".to_owned(),
    },
    services: ServicesSection::default(),
    cache: CacheSection::default(),
    workspace: WorkspaceSection::default(),
    shadow: ShadowSection::default(),
  };
  save_config(&config_path, &config)?;
  Ok(config_path)
}

/// Seed a per-repo registration at `<home>/runners/<owner>/<repo>/`.
fn seed_per_repo(home: &Path, owner: &str, repo: &str, name: &str) -> Result<PathBuf, RunnerError> {
  let dir = home.join("runners").join(owner).join(repo);
  write_registration(&dir, name, owner, repo)
}

/// Seed the legacy single-slot registration at `<home>/config.toml`.
fn seed_legacy(home: &Path, name: &str) -> Result<PathBuf, RunnerError> {
  write_registration(home, name, "legacyowner", "legacyrepo")
}

/// Run `git -C <cwd> <args>` and assert it succeeded.
fn run_git(cwd: &Path, args: &[&str]) -> Result<(), std::io::Error> {
  let output = std::process::Command::new("git")
    .arg("-C")
    .arg(cwd)
    .args(args)
    .output()?;
  assert!(
    output.status.success(),
    "git {args:?} failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  Ok(())
}

/// `git init` a fresh tempdir and point its `origin` remote at `remote`.
fn git_repo_with_origin(remote: &str) -> Result<tempfile::TempDir, std::io::Error> {
  let dir = tempfile::tempdir()?;
  run_git(dir.path(), &["init", "--quiet"])?;
  run_git(dir.path(), &["remote", "add", "origin", remote])?;
  Ok(dir)
}

/// AC-7 (legacy fallback): with only the legacy `<home>/config.toml`
/// present and a non-git cwd (no inference), `status` resolves the legacy
/// registration as the sole one.
#[test]
fn status_resolves_legacy_only_home_without_inference() {
  let home = tempfile::tempdir().expect("home tempdir");
  let cwd = tempfile::tempdir().expect("cwd tempdir");
  seed_legacy(home.path(), "legacy-runner").expect("seed legacy registration");

  let output = runner_cmd(home.path(), cwd.path())
    .arg("status")
    .output()
    .expect("spawn status");

  assert!(
    output.status.success(),
    "status must resolve the sole legacy registration; stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout.contains("legacy-runner"),
    "status must print the legacy registration: {stdout}"
  );
}

/// AC-7 (inference wins): a per-repo `runners/o1/r1/` registration AND the
/// legacy one both exist; from a git repo whose `origin` is o1/r1, `status`
/// picks the per-repo registration — no ambiguity error, legacy loses.
#[test]
fn status_prefers_cwd_inferred_registration_over_legacy() {
  let home = tempfile::tempdir().expect("home tempdir");
  seed_legacy(home.path(), "legacy-runner").expect("seed legacy registration");
  seed_per_repo(home.path(), "o1", "r1", "o1-r1-runner").expect("seed o1/r1 registration");
  let repo = git_repo_with_origin("https://github.com/o1/r1.git").expect("temp git repo");

  let output = runner_cmd(home.path(), repo.path())
    .arg("status")
    .output()
    .expect("spawn status");

  assert!(
    output.status.success(),
    "status must resolve via cwd inference; stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout.contains("o1-r1-runner"),
    "status must print the cwd-inferred o1/r1 registration: {stdout}"
  );
  assert!(
    !stdout.contains("legacy-runner"),
    "status must not fall back to the legacy registration: {stdout}"
  );
}

/// AC-9 (ambiguous): two per-repo registrations plus the legacy one, and a
/// cwd whose `origin` matches none of them — `status` exits non-zero and
/// the error lists every candidate (`o1/r1`, `o2/r2`, and `legacy`).
#[test]
fn status_with_ambiguous_registrations_lists_candidates() {
  let home = tempfile::tempdir().expect("home tempdir");
  seed_per_repo(home.path(), "o1", "r1", "r1-runner").expect("seed o1/r1 registration");
  seed_per_repo(home.path(), "o2", "r2", "r2-runner").expect("seed o2/r2 registration");
  seed_legacy(home.path(), "legacy-runner").expect("seed legacy registration");
  let repo = git_repo_with_origin("https://github.com/other/nomatch.git").expect("temp git repo");

  let output = runner_cmd(home.path(), repo.path())
    .arg("status")
    .output()
    .expect("spawn status");

  assert!(
    !output.status.success(),
    "ambiguous registrations must exit non-zero"
  );
  let stderr = String::from_utf8_lossy(&output.stderr);
  for candidate in ["o1/r1", "o2/r2", "legacy"] {
    assert!(
      stderr.contains(candidate),
      "ambiguity error must list {candidate}: {stderr}"
    );
  }
  assert!(
    stderr.contains("--config"),
    "ambiguity error must name the --config fix: {stderr}"
  );
}

/// AC-9 (sole): exactly one registration and an unrelated cwd (a git repo
/// whose `origin` matches no registration) still resolves — the sole
/// registration needs neither a flag nor a matching cwd.
#[test]
fn status_resolves_sole_registration_from_unrelated_cwd() {
  let home = tempfile::tempdir().expect("home tempdir");
  seed_per_repo(home.path(), "o1", "r1", "sole-runner").expect("seed o1/r1 registration");
  let repo = git_repo_with_origin("https://github.com/other/nomatch.git").expect("temp git repo");

  let output = runner_cmd(home.path(), repo.path())
    .arg("status")
    .output()
    .expect("spawn status");

  assert!(
    output.status.success(),
    "status must resolve the sole registration; stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout.contains("sole-runner"),
    "status must print the sole registration: {stdout}"
  );
}

/// AC-9 (zero): an empty home and a non-git cwd — the error names
/// `toolu-runner register` as the fix.
#[test]
fn status_with_no_registrations_names_register() {
  let home = tempfile::tempdir().expect("home tempdir");
  let cwd = tempfile::tempdir().expect("cwd tempdir");

  let output = runner_cmd(home.path(), cwd.path())
    .arg("status")
    .output()
    .expect("spawn status");

  assert!(
    !output.status.success(),
    "status with zero registrations must exit non-zero"
  );
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("toolu-runner register"),
    "error must name `toolu-runner register` as the fix: {stderr}"
  );
}

/// Seed the runtime files `remove` must handle next to a registration:
/// `credentials.json`, a plain-file `.lock` (no live holder), and a
/// `_diag/jobs/` journal file that must survive the removal.
fn seed_runtime_state(dir: &Path) -> Result<(), std::io::Error> {
  std::fs::write(
    dir.join("credentials.json"),
    "{\"access_token\":\"fixture-client-id\",\"issued_at\":\"2026-07-12T00:00:00+00:00\"}",
  )?;
  std::fs::write(dir.join(".lock"), "{\"pid\":0}")?;
  let jobs = dir.join("_diag").join("jobs");
  std::fs::create_dir_all(&jobs)?;
  std::fs::write(jobs.join("history.jsonl"), "{\"v\":1}\n")?;
  Ok(())
}

/// OQ-3: `remove` on a per-repo registration deletes `config.toml`,
/// `credentials.json`, the seeded `.lock` file, and any `.pending_remove`
/// marker, while `_diag/` and its contents survive for `watch` history.
/// The `.lock` is a plain seeded file with no live holder, so `--force`
/// takes the force-cancel branch and proceeds to delete state.
#[test]
fn remove_deletes_registration_files_but_keeps_diag() {
  let home = tempfile::tempdir().expect("home tempdir");
  let cwd = tempfile::tempdir().expect("cwd tempdir");
  let config_path =
    seed_per_repo(home.path(), "o1", "r1", "removed-runner").expect("seed o1/r1 registration");
  let repo_dir = config_path.parent().expect("per-repo dir").to_path_buf();
  seed_runtime_state(&repo_dir).expect("seed lock + credentials + _diag");

  let output = runner_cmd(home.path(), cwd.path())
    .args(["remove", "--force"])
    .output()
    .expect("spawn remove");

  assert!(
    output.status.success(),
    "remove --force must succeed; stderr: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  for gone in [
    "config.toml",
    "credentials.json",
    ".lock",
    ".pending_remove",
  ] {
    assert!(
      !repo_dir.join(gone).exists(),
      "{gone} must be deleted by remove (OQ-3)"
    );
  }
  assert!(
    repo_dir.join("_diag/jobs/history.jsonl").is_file(),
    "_diag contents must survive remove (watch history, OQ-3)"
  );
}
