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
  /// `None` in v1 (no cgroup isolation — runners run in the user's session).
  /// Reserved for a future v1.1 capability.
  pub cgroup_path: Option<PathBuf>,
}
