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
pub mod paths;

pub mod startup;

pub use config::RunnerConfig;
pub use error::RunnerError;
pub use events::{AnnotationLevel, Conclusion, ListenerEvent, LogStream, RunnerEvent};
pub use job_message::{
  ActionStep, ActionStepDefinitionReference, AgentJobRequestMessage, DictEntry, JobAuthorization,
  JobEndpoint, JobResources, MaskHint, PipelineContextData, TaskOrchestrationPlanReference,
  TemplateToken, VariableValue, WorkspaceOptions,
};
