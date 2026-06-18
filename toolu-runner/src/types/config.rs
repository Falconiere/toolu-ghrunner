use std::path::PathBuf;

/// Runner environment configuration — no job-specific data.
#[derive(Debug, Clone)]
pub struct RunnerConfig {
  /// Root directory for caches (actions, node, tools).
  pub data_dir: PathBuf,
  /// Parent directory for per-job workspace subdirectories.
  pub workspace_root: PathBuf,
  /// Per-job cgroup-v2 directory used to enforce CPU/memory limits.
  ///
  /// Set in Serve mode after the job cgroup is created; spawned step processes
  /// are moved into it. `None` in listener/JIT mode (no cgroup isolation).
  pub cgroup_path: Option<PathBuf>,
}
