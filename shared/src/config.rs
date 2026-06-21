use std::path::PathBuf;

/// How the runner serves artifacts / cache / OIDC to step actions.
///
/// - [`Forwarder`](ServicesMode::Forwarder) (default) copies the real
///   GitHub service URLs + runtime token out of the job message into step
///   env, so the JS toolkit `@v4` actions talk to real GitHub — matching the
///   official `actions/runner`.
/// - [`Offline`](ServicesMode::Offline) hosts local fake artifact/cache/OIDC
///   services and points step env at them, for hermetic / airgapped runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServicesMode {
  /// Forward real GitHub service URLs from the job message (default).
  #[default]
  Forwarder,
  /// Host local services and wire step env at them.
  Offline,
}

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
  /// Artifact/cache/OIDC serving mode (`forwarder` default, or `offline`).
  pub services_mode: ServicesMode,
}
