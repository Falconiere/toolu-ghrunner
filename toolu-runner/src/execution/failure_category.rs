//! Map a `RunnerError` to a `FailureCategory` (light edit from yamless).
//!
//! yamless used a separate `yamless_shared::diagnostics::FailureCategory` enum.
//! In toolu we re-export the same variant set from the `execution` module so
//! the call sites in `job_runner` keep working without depending on
//! `yamless-shared`.
//!
//! `#[allow(dead_code)]` because the diagnostic emission (`emit_diagnostic`
//! in yamless) was a yamless-only concern — the category types are kept for
//! forward compatibility but no runner code currently consumes them.

#![allow(dead_code)]

use shared::RunnerError;

/// Diagnostic category for a `RunnerError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureCategory {
  Docker,
  Network,
  Infrastructure,
  Auth,
  StepExecution,
  Internal,
}

impl FailureCategory {
  pub fn name(self) -> &'static str {
    match self {
      Self::Docker => "docker",
      Self::Network => "network",
      Self::Infrastructure => "infrastructure",
      Self::Auth => "auth",
      Self::StepExecution => "step-execution",
      Self::Internal => "internal",
    }
  }
}

/// Classify a `RunnerError` into the `FailureCategory` it belongs to.
pub(super) fn category_from_runner_error(err: &RunnerError) -> FailureCategory {
  match err {
    RunnerError::Docker(_) => FailureCategory::Docker,
    RunnerError::Network(_) => FailureCategory::Network,
    RunnerError::WorkspaceInit { .. } | RunnerError::Io(_) => FailureCategory::Infrastructure,
    RunnerError::Auth(_) | RunnerError::Oidc(_) => FailureCategory::Auth,
    RunnerError::StepExecution(_)
    | RunnerError::ScriptHandler(_)
    | RunnerError::Expression(_)
    | RunnerError::FileCommand(_) => FailureCategory::StepExecution,
    RunnerError::Protocol(_)
    | RunnerError::ActionResolution(_)
    | RunnerError::ActionDownload(_)
    | RunnerError::ActionManifest(_)
    | RunnerError::NodeRuntime(_)
    | RunnerError::NodeHandler(_)
    | RunnerError::Artifact(_)
    | RunnerError::Cache(_)
    | RunnerError::ReusableWorkflow(_)
    | RunnerError::Reporting(_)
    | RunnerError::Config(_)
    | RunnerError::Cancelled
    | RunnerError::Json(_) => FailureCategory::Internal,
  }
}
