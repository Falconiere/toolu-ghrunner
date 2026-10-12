//! Cross-cutting types and startup utilities for toolu-runner.
//!
//! This crate is the smallest of the three: types, error enum, and the
//! tracing init. No async, no I/O beyond local file paths in the startup
//! module.

#![doc(html_root_url = "https://docs.rs/shared/0.1.0")]

mod config;
mod error;
mod events;
mod job_message;
/// Credential redaction for proxy URLs (`[network] proxy`, proxy env vars).
pub mod network_redact;
/// Home-directory and tilde path resolution.
pub mod paths;
/// Runner OS/arch reporting (GitHub `RUNNER_OS` / `RUNNER_ARCH`).
pub mod platform;
mod secret_masker;

/// Tracing initialization and secret redaction.
pub mod startup;

pub use config::{
  CacheConfig, L2Config, NetworkConfig, RunnerConfig, ServicesMode, WorkspaceConfig, WorkspaceMode,
};
pub use error::{ActionFetchError, ActionFetchKind, RunnerError, SESSION_CONFLICT_EXIT_CODE};
pub use events::{
  AnnotationLevel, Conclusion, ListenerEvent, LogStream, RunnerEvent, SETUP_STEP_ID,
};
pub use job_message::{
  ActionStep, ActionStepDefinitionReference, ActionsEnvironment, AgentJobRequestMessage, DictEntry,
  JobAuthorization, JobEndpoint, JobResources, MaskHint, PipelineContextData,
  TaskOrchestrationPlanReference, TemplateToken, VariableValue, WorkspaceOptions,
};
pub use secret_masker::{MaskerRedactor, SecretMasker};
