//! Tests for `validate` against real binaries: this host's `node`, a real
//! non-Node system binary, a missing path and a non-executable file.

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The host's real `node` on `PATH` and its major. A host without Node fails
/// the test: an unverified runtime check is not a passing one.
fn host_node() -> Result<(std::path::PathBuf, u8), Box<dyn std::error::Error>> {
  let path = std::env::var_os("PATH").ok_or("PATH unset")?;
  let node = std::env::split_paths(&path)
    .map(|dir| dir.join("node"))
    .find(|candidate| candidate.is_file())
    .ok_or("no `node` on PATH; the runtime tests need a real Node.js")?;
  let output = std::process::Command::new(&node)
    .arg("--version")
    .output()?;
  let version = String::from_utf8(output.stdout)?;
  let major = version
    .trim()
    .trim_start_matches('v')
    .split('.')
    .next()
    .ok_or("empty node version")?
    .parse()?;
  Ok((node, major))
}

#[tokio::test]
async fn real_host_node_passes_for_its_own_major() -> TestResult {
  let (node, major) = host_node()?;
  validate_runtime(&node, major).await?;
  Ok(())
}

#[tokio::test]
async fn real_host_node_fails_for_another_major() -> TestResult {
  let (node, major) = host_node()?;
  let other = major.wrapping_add(1);
  let error = validate_runtime(&node, other).await;
  let message = error.err().ok_or("must fail")?;
  assert!(
    message.contains(&format!("expected v{other}.x")) && message.contains("reported version"),
    "{message}"
  );
  Ok(())
}

#[tokio::test]
#[cfg(unix)]
async fn a_real_non_node_binary_is_rejected() -> TestResult {
  // GNU echo prints its coreutils banner; BSD echo prints "--version".
  let error = validate_runtime(Path::new("/bin/echo"), 20).await;
  let message = error.err().ok_or("must fail")?;
  assert!(message.contains("/bin/echo reported version"), "{message}");
  Ok(())
}

#[tokio::test]
async fn a_missing_or_non_executable_runtime_cannot_start() -> TestResult {
  let dir = tempfile::tempdir()?;
  let missing = dir.path().join("bin/node");
  let error = validate_runtime(&missing, 20).await;
  let message = error.err().ok_or("must fail")?;
  assert!(
    message.contains("failed to start on this host"),
    "{message}"
  );

  let not_executable = dir.path().join("node");
  std::fs::write(&not_executable, b"#!/bin/sh\necho v20.20.2\n")?;
  let error = validate_runtime(&not_executable, 20).await;
  let message = error.err().ok_or("must fail")?;
  assert!(
    message.contains("failed to start on this host"),
    "{message}"
  );
  Ok(())
}
