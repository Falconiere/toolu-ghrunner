/// Errors produced by the runner execution engine.
#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
  #[error("expression evaluation failed: {0}")]
  Expression(String),
  #[error("step execution failed: {0}")]
  StepExecution(String),
  #[error("script handler error: {0}")]
  ScriptHandler(String),
  #[error("file command error: {0}")]
  FileCommand(String),
  #[error("protocol error: {0}")]
  Protocol(String),
  #[error("action resolution failed: {0}")]
  ActionResolution(String),
  #[error("action download failed: {0}")]
  ActionDownload(String),
  #[error("action manifest error: {0}")]
  ActionManifest(String),
  #[error("node runtime error: {0}")]
  NodeRuntime(String),
  #[error("node handler error: {0}")]
  NodeHandler(String),
  #[error("docker error: {0}")]
  Docker(String),
  #[error("OIDC error: {0}")]
  Oidc(String),
  #[error("artifact service error: {0}")]
  Artifact(String),
  #[error("cache service error: {0}")]
  Cache(String),
  #[error("reusable workflow error: {0}")]
  ReusableWorkflow(String),
  #[error("reporting error: {0}")]
  Reporting(String),
  #[error("auth error: {0}")]
  Auth(String),
  #[error("network error: {0}")]
  Network(String),
  #[error("config error: {0}")]
  Config(String),
  #[error("workspace init failed at {path}: {source}")]
  WorkspaceInit {
    path: std::path::PathBuf,
    source: std::io::Error,
  },
  #[error("job cancelled")]
  Cancelled,
  #[error("IO error: {0}")]
  Io(#[from] std::io::Error),
  #[error("JSON error: {0}")]
  Json(#[from] serde_json::Error),
}
