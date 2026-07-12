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
    "TOOLU_RUNNER_LOG",
    "TOOLU_RUNNER_ALLOW_VERBOSE",
  ] {
    assert!(stdout.contains(var), "missing {var} in: {stdout}");
  }
}

#[test]
fn register_help_lists_required_flags() {
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
fn config_flag_help_states_default_path_everywhere() {
  for subcommand in [
    "register", "run", "remove", "status", "watch", "login", "logout",
  ] {
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
      stdout.contains("config.toml"),
      "{subcommand} --help does not state the --config default: {stdout}"
    );
  }
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
