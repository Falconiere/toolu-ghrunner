/// Errors produced by the runner execution engine.
#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
  /// A `${{ }}` expression failed to evaluate.
  #[error("expression evaluation failed: {0}")]
  Expression(String),
  /// A workflow step failed to execute.
  #[error("step execution failed: {0}")]
  StepExecution(String),
  /// The `script` (shell) handler failed.
  #[error("script handler error: {0}")]
  ScriptHandler(String),
  /// A workflow command file (e.g. `GITHUB_ENV`) could not be processed.
  #[error("file command error: {0}")]
  FileCommand(String),
  /// A GitHub Actions protocol message was malformed or unexpected.
  #[error("protocol error: {0}")]
  Protocol(String),
  /// An action reference could not be resolved to a downloadable source.
  #[error("action resolution failed: {0}")]
  ActionResolution(String),
  /// An action's source could not be downloaded.
  #[error("action download failed: {0}")]
  ActionDownload(String),
  /// An action fetch failed for a runner-infrastructure reason.
  #[error("action download failed: {0}")]
  ActionFetch(ActionFetchError),
  /// An action's `action.yml` manifest was invalid or unreadable.
  #[error("action manifest error: {0}")]
  ActionManifest(String),
  /// The Node.js runtime could not be provisioned or invoked.
  #[error("node runtime error: {0}")]
  NodeRuntime(String),
  /// The `node` / `node_exec` handler failed.
  #[error("node handler error: {0}")]
  NodeHandler(String),
  /// The `docker` handler (bollard) failed.
  #[error("docker error: {0}")]
  Docker(String),
  /// The OIDC token server or claims handling failed.
  #[error("OIDC error: {0}")]
  Oidc(String),
  /// Artifact upload/download failed.
  #[error("artifact service error: {0}")]
  Artifact(String),
  /// The content-addressed cache service failed.
  #[error("cache service error: {0}")]
  Cache(String),
  /// Reusable workflow resolution failed.
  #[error("reusable workflow error: {0}")]
  ReusableWorkflow(String),
  /// Reporting status/logs back to the Results Service failed.
  #[error("reporting error: {0}")]
  Reporting(String),
  /// Authentication failed (e.g. a rejected JIT bearer) — fatal for the run loop.
  #[error("auth error: {0}")]
  Auth(String),
  /// A transient network failure occurred; the run loop backs off and retries.
  #[error("network error: {0}")]
  Network(String),
  /// The runner's on-disk configuration was invalid or unreadable.
  #[error("config error: {0}")]
  Config(String),
  /// The per-job workspace directory could not be created or written.
  #[error("workspace init failed at {path}: {source}")]
  WorkspaceInit {
    /// The workspace path that failed to initialize.
    path: std::path::PathBuf,
    /// The underlying I/O error.
    source: std::io::Error,
  },
  /// Another `run` process already holds the per-repo lock for this registration.
  #[error("runner is already running as PID {pid} (started {started_at}) — exiting")]
  LockHeld {
    /// PID of the process holding the lock.
    pid: u32,
    /// Timestamp (as recorded in the lock file) when the lock holder started.
    started_at: String,
    /// Path to the config file the lock holder is running with.
    config_path: String,
  },
  /// The job was cancelled (e.g. via `SIGINT`/`SIGTERM`).
  #[error("job cancelled")]
  Cancelled,
  /// A wrapped `std::io::Error`.
  #[error("IO error: {0}")]
  Io(#[from] std::io::Error),
  /// A wrapped `serde_json::Error`.
  #[error("JSON error: {0}")]
  Json(#[from] serde_json::Error),
}

/// Why an action fetch failed, typed where the status and phase are known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionFetchKind {
  /// The download-info service failed (not a user error such as a missing repo).
  ResolveService,
  /// The archive request returned this non-success HTTP status.
  ArchiveStatus(u16),
  /// The archive transfer failed (transport, timeout, redirect, mid-body stream).
  ArchiveTransport,
  /// The archive content is not a valid gzip/tar.
  ArchiveContent,
}

/// A typed action fetch failure; Display is the message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ActionFetchError {
  /// The failure class.
  pub kind: ActionFetchKind,
  /// Human-readable detail (upstream's inner exception message).
  pub message: String,
}

impl ActionFetchError {
  /// Upstream `infrastructureFailureCategory` for this failure, or `None`
  /// when the failure is the user's (an archive 403 is access denied).
  pub fn infrastructure_category(&self) -> Option<&'static str> {
    match self.kind {
      ActionFetchKind::ResolveService => Some("resolve_action"),
      ActionFetchKind::ArchiveStatus(403) => None,
      ActionFetchKind::ArchiveStatus(_) | ActionFetchKind::ArchiveTransport => {
        Some("error_download_action")
      },
      ActionFetchKind::ArchiveContent => Some("invalid_action_download"),
    }
  }
}
