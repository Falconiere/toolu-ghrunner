//! CLI smoke tests for `toolu-runner`.
//!
//! Builds the binary via `cargo run` and exercises the clap surface.
//! These tests are integration-level — they shell out to `cargo run`
//! because the runner's CLI shape is the contract being verified.
//! Every invocation also runs `cli::debug_assert_cli()` (clap's own
//! definition self-check, wired at startup in debug builds), so a
//! conflicting or invalid arg definition fails these tests.

use std::process::Command;

fn toolu_runner() -> Command {
  let mut cmd = Command::new(env!("CARGO"));
  cmd.args(["run", "-p", "toolu-runner", "--quiet", "--"]);
  cmd
}

#[test]
fn help_lists_all_subcommands() {
  let output = toolu_runner()
    .arg("--help")
    .output()
    .expect("should run toolu-runner --help");

  assert!(output.status.success(), "--help should exit cleanly");
  let stdout = String::from_utf8_lossy(&output.stdout);
  for subcommand in [
    "register", "run", "remove", "status", "watch", "login", "logout",
  ] {
    assert!(
      stdout.contains(subcommand),
      "missing {subcommand} in: {stdout}"
    );
  }
}

#[test]
fn top_level_help_shows_examples_and_env_vars() {
  let output = toolu_runner()
    .arg("--help")
    .output()
    .expect("should run toolu-runner --help");

  assert!(output.status.success(), "--help should exit cleanly");
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout.contains("Examples:"),
    "missing Examples in: {stdout}"
  );
  assert!(
    stdout.contains("Environment:"),
    "missing Environment in: {stdout}"
  );
  for var in [
    "TOOLU_RUNNER_TOKEN",
    "TOOLU_RUNNER_CLIENT_ID",
    "TOOLU_RUNNER_HOME",
    "TOOLU_RUNNER_LOG",
    "TOOLU_RUNNER_ALLOW_VERBOSE",
  ] {
    assert!(stdout.contains(var), "missing {var} in: {stdout}");
  }
  assert!(
    stdout.contains("repo inferred from the cwd git remote"),
    "missing zero-arg register example in: {stdout}"
  );
}

#[test]
fn register_help_lists_flags() {
  let output = toolu_runner()
    .args(["register", "--help"])
    .output()
    .expect("should run toolu-runner register --help");

  assert!(
    output.status.success(),
    "register --help should exit cleanly"
  );
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(stdout.contains("--url"), "missing --url in: {stdout}");
  assert!(stdout.contains("--token"), "missing --token in: {stdout}");
  assert!(stdout.contains("--name"), "missing --name in: {stdout}");
  assert!(stdout.contains("--labels"), "missing --labels in: {stdout}");
}

#[test]
fn register_help_shows_url_optional_with_inference() {
  let output = toolu_runner()
    .args(["register", "--help"])
    .output()
    .expect("should run toolu-runner register --help");

  assert!(
    output.status.success(),
    "register --help should exit cleanly"
  );
  let stdout = String::from_utf8_lossy(&output.stdout);
  // An optional --url is absent from the usage line; clap appends
  // required args after [OPTIONS].
  assert!(
    stdout.contains("Usage: toolu-runner register [OPTIONS]"),
    "missing optional-args usage line in: {stdout}"
  );
  assert!(
    !stdout.contains("[OPTIONS] --url"),
    "--url must not be required in the usage line: {stdout}"
  );
  assert!(
    stdout.contains("inferred from the cwd git remote `origin` (github.com only)"),
    "--url help does not document cwd inference: {stdout}"
  );
  assert!(
    stdout.contains("cd my-repo && toolu-runner register"),
    "missing zero-arg example in: {stdout}"
  );
}

#[test]
fn register_help_documents_token_resolution_and_single_use() {
  let output = toolu_runner()
    .args(["register", "--help"])
    .output()
    .expect("should run toolu-runner register --help");

  assert!(
    output.status.success(),
    "register --help should exit cleanly"
  );
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout.contains("single-use"),
    "missing single-use warning in: {stdout}"
  );
  assert!(
    stdout.contains("TOOLU_RUNNER_TOKEN"),
    "missing TOOLU_RUNNER_TOKEN fallback in: {stdout}"
  );
}

#[test]
fn login_help_documents_client_id_env_fallback() {
  let output = toolu_runner()
    .args(["login", "--help"])
    .output()
    .expect("should run toolu-runner login --help");

  assert!(output.status.success(), "login --help should exit cleanly");
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout.contains("TOOLU_RUNNER_CLIENT_ID"),
    "missing TOOLU_RUNNER_CLIENT_ID fallback in: {stdout}"
  );
}

#[test]
fn config_flag_help_states_default_resolution_everywhere() {
  for subcommand in ["register", "run", "remove", "status", "watch"] {
    let output = toolu_runner()
      .args([subcommand, "--help"])
      .output()
      .expect("should run toolu-runner <subcommand> --help");

    assert!(
      output.status.success(),
      "{subcommand} --help should exit cleanly"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
      stdout.contains("inferred from the cwd git remote"),
      "{subcommand} --help does not state the inferred --config default: {stdout}"
    );
    assert!(
      stdout.contains("sole existing registration"),
      "{subcommand} --help does not state the sole-registration fallback: {stdout}"
    );
  }
  // login/logout dropped --config entirely: the token store is pinned to
  // the runner home (TOOLU_RUNNER_HOME > ~/.toolu-runner), shared by all
  // per-repo registrations.
  for subcommand in ["login", "logout"] {
    let output = toolu_runner()
      .args([subcommand, "--help"])
      .output()
      .expect("should run toolu-runner <subcommand> --help");

    assert!(
      output.status.success(),
      "{subcommand} --help should exit cleanly"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
      !stdout.contains("--config"),
      "{subcommand} --help must not offer --config (token store lives at the runner home): {stdout}"
    );
  }
}

#[test]
fn run_without_config_reports_missing_registration() {
  // cwd pinned to the temp home (not a git repo) so cwd inference stays
  // deterministic-off and the resolver reaches the zero-registrations case.
  let home = tempfile::tempdir().expect("tempdir");
  let output = Command::new(env!("CARGO_BIN_EXE_toolu-runner"))
    .env("HOME", home.path())
    .env_remove("TOOLU_RUNNER_HOME")
    .current_dir(home.path())
    .arg("run")
    .output()
    .expect("should run toolu-runner run");

  assert!(!output.status.success(), "run without a config must fail");
  let stderr = String::from_utf8_lossy(&output.stderr);
  let expected_home = home.path().join(".toolu-runner");
  assert!(
    stderr.contains(&*expected_home.to_string_lossy()),
    "error should name the runner home {}: {stderr}",
    expected_home.display()
  );
  assert!(
    stderr.contains("toolu-runner register"),
    "error should name `toolu-runner register` as the fix: {stderr}"
  );
}

#[test]
fn run_reports_missing_credentials_next_to_config() {
  let dir = tempfile::tempdir().expect("tempdir");
  let config_path = dir.path().join("config.toml");
  std::fs::write(&config_path, "").expect("write placeholder config");

  let output = Command::new(env!("CARGO_BIN_EXE_toolu-runner"))
    .env("HOME", dir.path())
    .args(["run", "--config"])
    .arg(&config_path)
    .output()
    .expect("should run toolu-runner run --config");

  assert!(
    !output.status.success(),
    "run without credentials must fail"
  );
  let stderr = String::from_utf8_lossy(&output.stderr);
  let expected = dir.path().join("credentials.json");
  assert!(
    stderr.contains(&*expected.to_string_lossy()),
    "error should name the sibling credentials path {}: {stderr}",
    expected.display()
  );
}

#[test]
fn clap_self_check_runs_at_startup() {
  // `main` calls `cli::debug_assert_cli()` before parsing when
  // debug_assertions are on. `cargo test` builds this binary with the dev
  // profile, so an invalid clap definition panics before `--version` can
  // print — a clean exit here IS the assertion that clap's
  // `Command::debug_assert` self-check passed. (Under `--release` the
  // check is compiled out and this test only asserts `--version` works.)
  let output = Command::new(env!("CARGO_BIN_EXE_toolu-runner"))
    .arg("--version")
    .output()
    .expect("should run toolu-runner --version");

  assert!(
    output.status.success(),
    "startup clap self-check must pass: {}",
    String::from_utf8_lossy(&output.stderr)
  );
}

#[test]
fn version_prints_package_version() {
  let output = toolu_runner()
    .arg("--version")
    .output()
    .expect("should run toolu-runner --version");

  assert!(output.status.success(), "--version should exit cleanly");
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout.contains(env!("CARGO_PKG_VERSION")),
    "expected version {} in: {stdout}",
    env!("CARGO_PKG_VERSION"),
  );
}
