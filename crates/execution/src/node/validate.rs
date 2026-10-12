//! Startup check for a Node.js runtime: it must run on this host and report
//! the requested major. Catches a wrong-libc build (e.g. a glibc Node on a
//! musl host), a non-executable or non-Node file, and a mislabeled preseed.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

/// Upper bound for `node --version`, generous for a cold disk.
const VERSION_TIMEOUT: Duration = Duration::from_secs(30);

/// Run `<binary> --version` and require exit 0 with output `v{major}.…`.
///
/// # Errors
///
/// Returns a message naming `binary` when it cannot start, times out, exits
/// non-zero, or reports another version.
pub async fn validate_runtime(binary: &Path, major: u8) -> Result<(), String> {
  let fail = |cause: String| {
    format!(
      "Node.js runtime {} failed to start on this host: {cause}",
      binary.display()
    )
  };
  let child = tokio::process::Command::new(binary)
    .arg("--version")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true)
    .output();
  let output = tokio::time::timeout(VERSION_TIMEOUT, child)
    .await
    .map_err(|elapsed| {
      fail(format!(
        "`--version` did not finish within {VERSION_TIMEOUT:?} ({elapsed})"
      ))
    })?
    .map_err(|e| fail(e.to_string()))?;
  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    return Err(fail(format!(
      "`--version` exited with {}: {}",
      output.status,
      stderr.trim()
    )));
  }
  let reported = String::from_utf8_lossy(&output.stdout).trim().to_owned();
  let expected = format!("v{major}.");
  if reported.starts_with(&expected) {
    return Ok(());
  }
  Err(format!(
    "Node.js runtime {} reported version {reported:?}, expected v{major}.x",
    binary.display()
  ))
}

#[cfg(test)]
#[path = "tests/validate.rs"]
mod tests;
