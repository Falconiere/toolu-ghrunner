//! Spawn step processes and join them into their per-job cgroup (v2).
//!
//! Per-job cgroups carry cpu.max/memory.max limits, but those limits only bind
//! once a process is written into `<cgroup>/cgroup.procs`. This helper spawns a
//! command and, when a cgroup path is provided, moves the child into it so the
//! limits actually take effect. Joining is best-effort: a failed write is logged
//! and execution continues (handled degradation, not silent suppression).

use std::path::Path;

use shared::RunnerError;
use tokio::process::{Child, Command};

/// Spawn `cmd` and, when `cgroup_path` is `Some`, move the child into that
/// cgroup by writing its PID to `<cgroup_path>/cgroup.procs`.
///
/// In listener/JIT mode there is no per-job cgroup, so `None` skips the join and
/// the child runs unconstrained. A failed cgroup write is logged via
/// `tracing::warn!` and the (already-spawned) child is returned regardless, so
/// callers keep their existing wait/stream logic.
///
/// # Errors
///
/// Returns `RunnerError::Io` if the command itself cannot be spawned.
pub async fn spawn_in_cgroup(
  cmd: &mut Command,
  cgroup_path: Option<&Path>,
) -> Result<Child, RunnerError> {
  let child = cmd.spawn().map_err(RunnerError::Io)?;

  if let Some(path) = cgroup_path {
    join_cgroup(&child, path);
  }

  Ok(child)
}

/// Best-effort: write the child PID into `<path>/cgroup.procs`.
///
/// A missing PID or a failed write is logged and ignored — enforcement is
/// best-effort and must never abort an already-running step.
fn join_cgroup(child: &Child, path: &Path) {
  let cgroup = path.display().to_string();
  let Some(pid) = child.id() else {
    warn_no_pid(&cgroup);
    return;
  };
  match write_pid(path, pid) {
    Ok(()) => tracing::debug!(pid, cgroup, "joined step into cgroup"),
    Err(e) => warn_join_failed(&cgroup, pid, &e),
  }
}

/// Write `pid` into `<path>/cgroup.procs`, moving the process into the cgroup.
fn write_pid(path: &Path, pid: u32) -> std::io::Result<()> {
  std::fs::write(path.join("cgroup.procs"), pid.to_string())
}

fn warn_no_pid(cgroup: &str) {
  tracing::warn!(cgroup, "spawned child has no PID; cannot join cgroup");
}

fn warn_join_failed(cgroup: &str, pid: u32, error: &std::io::Error) {
  tracing::warn!(
    pid,
    cgroup,
    error = %error,
    "failed to join step into cgroup; limits not enforced for this step"
  );
}
