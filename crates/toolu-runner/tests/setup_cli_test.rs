//! CLI smoke tests for the `setup` wizard subcommand.
//!
//! These shell out to the built binary (like `cli_test.rs`) because the
//! wizard's terminal-vs-non-terminal behavior and its `--help` surface are
//! the contract under test. The full-screen TUI itself is only reachable on a
//! real terminal, so these cover the two headless-observable paths: the
//! non-interactive guard and the help text.

use std::process::Command;

/// AC-1: without a terminal (piped stdin/stderr, the default under
/// `Command::output`), `setup` must fail with the scriptable-fallback
/// guidance and must NOT switch to the alternate screen.
#[test]
fn setup_without_tty_fails_with_scriptable_guidance() {
  let output = Command::new(env!("CARGO_BIN_EXE_toolu-runner"))
    .arg("setup")
    .output()
    .expect("should run toolu-runner setup");

  assert!(
    !output.status.success(),
    "setup without a terminal must fail"
  );
  let stderr = String::from_utf8_lossy(&output.stderr);
  for word in ["login", "register", "install-service"] {
    assert!(
      stderr.contains(word),
      "guard message should name `{word}`: {stderr}"
    );
  }
  // The guard fires before any terminal setup, so the alternate-screen enter
  // sequence must never be emitted.
  let combined = format!("{}{}", String::from_utf8_lossy(&output.stdout), stderr);
  assert!(
    !combined.contains("\x1b[?1049h"),
    "setup must not enter the alternate screen when it bails on no-tty: {combined:?}"
  );
}

/// AC-11: `setup --help` exits cleanly and documents the github.com-only
/// scope plus the wizard's flags.
#[test]
fn setup_help_documents_github_only_and_flags() {
  let output = Command::new(env!("CARGO_BIN_EXE_toolu-runner"))
    .args(["setup", "--help"])
    .output()
    .expect("should run toolu-runner setup --help");

  assert!(output.status.success(), "setup --help should exit cleanly");
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout.contains("github.com"),
    "setup --help should state github.com only: {stdout}"
  );
  for flag in [
    "--url",
    "--token",
    "--name",
    "--labels",
    "--config",
    "--client-id",
  ] {
    assert!(stdout.contains(flag), "missing {flag} in: {stdout}");
  }
}
