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
