//! Bounded command capture shared by the live job-container comparison test.
#![cfg(feature = "live")]

use std::error::Error;
use std::fs;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Bounded subprocess output held in temporary files while a command runs.
pub struct Captured {
  /// Process exit status.
  pub status: ExitStatus,
  /// Captured standard output bytes.
  pub stdout: Vec<u8>,
  /// Captured standard error bytes.
  pub stderr: Vec<u8>,
}

/// Run one command with file-backed output and a 30-second execution bound.
///
/// # Errors
/// Returns an error on spawn, wait, output-read failure, or timeout.
pub fn capture(mut command: Command, description: &str) -> Result<Captured, Box<dyn Error>> {
  let stdout = tempfile::NamedTempFile::new()?;
  let stderr = tempfile::NamedTempFile::new()?;
  command
    .stdout(Stdio::from(stdout.reopen()?))
    .stderr(Stdio::from(stderr.reopen()?));
  let mut child = command
    .spawn()
    .map_err(|error| format!("could not start {description}: {error}"))?;
  let until = Instant::now() + COMMAND_TIMEOUT;
  let status = loop {
    if let Some(status) = child.try_wait()? {
      break status;
    }
    if Instant::now() >= until {
      let _ = child.kill();
      let _ = child.wait();
      return Err(
        format!("{description} exceeded the {COMMAND_TIMEOUT:?} subprocess bound").into(),
      );
    }
    thread::sleep(Duration::from_millis(50));
  };
  let stdout = fs::read(stdout.path())?;
  let stderr = fs::read(stderr.path())?;
  Ok(Captured {
    status,
    stdout,
    stderr,
  })
}
