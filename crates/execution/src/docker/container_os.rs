//! Which Node.js build a job container can start, decided like upstream
//! (`StepHost.DetermineNodeRuntimeVersion`): `sh -c "cat /etc/*release | grep
//! ^ID"` runs once per job and any `alpine` line selects the musl runtime.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use shared::{Conclusion, RunnerError};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::container_exec::ContainerExec;
use super::job_container::JobContainer;
use crate::node::platform::{NodeTarget, OS_RELEASE_PROBE, container_target, host_arch};

/// Bound for the probe; a hung container must not stall the step forever.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

impl JobContainer {
  /// The Node.js target for actions executed in this container.
  ///
  /// # Errors
  ///
  /// Returns a Docker error when the probe cannot be created or attached, or
  /// upstream's error for an Alpine container on a non-x64 runner.
  pub(crate) async fn node_target(
    &self,
    cancel: &CancellationToken,
  ) -> Result<NodeTarget, RunnerError> {
    let stdout = self
      .os_release
      .get_or_try_init(|| self.probe_os_release(cancel))
      .await?;
    container_target(stdout.as_deref(), host_arch()?)
  }

  /// Run the probe, returning its stdout when it exited zero and `None` when it
  /// exited non-zero (upstream's "best effort": glibc). A timeout or a
  /// cancellation is an error, so `get_or_try_init` never caches it.
  async fn probe_os_release(
    &self,
    cancel: &CancellationToken,
  ) -> Result<Option<String>, RunnerError> {
    let args = ["-c".to_owned(), OS_RELEASE_PROBE.to_owned()];
    let env = HashMap::new();
    let exec = ContainerExec {
      program: Path::new("sh"),
      args: &args,
      env: &env,
      working_dir: Path::new("/"),
      step_id: "",
      // Bounded below, so a timeout is an error rather than a cached "glibc".
      timeout: None,
      cancel,
    };
    let (stdout_tx, mut stdout_rx) = mpsc::channel::<String>(64);
    // Probe stderr is not step output: drain and drop it.
    let (events_tx, mut events_rx) = mpsc::channel(64);
    let run = async {
      let conclusion =
        tokio::time::timeout(PROBE_TIMEOUT, self.execute(&exec, &events_tx, stdout_tx)).await;
      drop(events_tx);
      conclusion
    };
    let collect = async {
      let mut stdout = String::new();
      while let Some(line) = stdout_rx.recv().await {
        stdout.push_str(&line);
        stdout.push('\n');
      }
      stdout
    };
    let drain = async { while events_rx.recv().await.is_some() {} };
    let (conclusion, stdout, ()) = tokio::join!(run, collect, drain);
    let conclusion = conclusion.map_err(|elapsed| {
      RunnerError::Docker(format!(
        "container OS probe did not finish within {PROBE_TIMEOUT:?} ({elapsed})"
      ))
    })??;
    // Only a probe that really ran decides; cancellation is not cached as glibc.
    match conclusion {
      Conclusion::Success => Ok(Some(stdout)),
      Conclusion::Failure | Conclusion::Skipped => Ok(None),
      Conclusion::Cancelled => Err(RunnerError::Docker(
        "container OS probe was cancelled".to_owned(),
      )),
    }
  }
}
