//! CLI smoke tests for `toolu-runner`.
//!
//! Builds the binary via `cargo run` and exercises the clap surface.
//! These tests are integration-level — they shell out to `cargo run`
//! because the runner's CLI shape is the contract being verified.

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
  assert!(stdout.contains("register"), "missing register in: {stdout}");
  assert!(stdout.contains("run"), "missing run in: {stdout}");
  assert!(stdout.contains("remove"), "missing remove in: {stdout}");
  assert!(stdout.contains("status"), "missing status in: {stdout}");
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
