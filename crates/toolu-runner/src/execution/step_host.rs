use std::collections::HashMap;
use std::path::{Path, PathBuf};

use shared::RunnerError;

use super::cgroup_join::spawn_in_cgroup;

/// Result of running a script via a StepHost.
pub struct ScriptOutput {
  pub exit_code: i64,
  pub stdout: String,
  pub stderr: String,
}

/// Abstraction for where `run:` steps are executed.
///
/// `DirectHost` spawns a local process. `ContainerHost` uses `docker exec`.
#[async_trait::async_trait]
pub trait StepHost: Send + Sync {
  /// Run a script using the given shell and arguments.
  async fn run_script(
    &self,
    shell: &str,
    args: &[&str],
    env: &HashMap<String, String>,
    working_dir: &Path,
  ) -> Result<ScriptOutput, RunnerError>;
}

/// Runs steps as local processes (default when no job container is set).
///
/// `cgroup_path` is the per-job cgroup-v2 directory spawned steps are moved into
/// for resource enforcement; `None` disables the join (listener/JIT mode).
#[derive(Default)]
pub struct DirectHost {
  pub cgroup_path: Option<PathBuf>,
}

#[async_trait::async_trait]
impl StepHost for DirectHost {
  async fn run_script(
    &self,
    shell: &str,
    args: &[&str],
    env: &HashMap<String, String>,
    working_dir: &Path,
  ) -> Result<ScriptOutput, RunnerError> {
    let mut cmd = tokio::process::Command::new(shell);
    cmd
      .args(args)
      .current_dir(working_dir)
      .env_clear()
      .envs(env)
      .stdout(std::process::Stdio::piped())
      .stderr(std::process::Stdio::piped());

    let child = spawn_in_cgroup(&mut cmd, self.cgroup_path.as_deref()).await?;
    let output = child
      .wait_with_output()
      .await
      .map_err(|e| RunnerError::ScriptHandler(format!("wait {shell}: {e}")))?;

    let exit_code = i64::from(output.status.code().unwrap_or(-1));

    Ok(ScriptOutput {
      exit_code,
      stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
      stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
  }
}
