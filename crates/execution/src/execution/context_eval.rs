//! Expression context snapshots derived from the live execution context.

use std::collections::HashMap;

use expressions::evaluator::{EvalContext, JobStatus};
use expressions::types::ExprValue;

use super::super::step_state::build_steps_context;
use super::{ExecutionContext, job_status_str, string_map_to_obj};

impl ExecutionContext {
  fn github_expression_context(&self) -> ExprValue {
    let mut github = match self.scoped_github_context() {
      ExprValue::Object(github) => github,
      ExprValue::Null
      | ExprValue::Bool(_)
      | ExprValue::Number(_)
      | ExprValue::String(_)
      | ExprValue::Array(_) => HashMap::new(),
    };
    if let Some(container) = &self.container {
      for key in ["workspace", "action_path"] {
        if let Some(ExprValue::String(value)) = github.get_mut(key) {
          *value = container.translate_value(value);
        }
      }
    }
    ExprValue::Object(github)
  }

  fn env_expression_context(&self) -> ExprValue {
    let env_obj: HashMap<String, ExprValue> = self
      .visible_env()
      .iter()
      .map(|(key, value)| {
        (
          key.clone(),
          ExprValue::String(self.container.as_ref().map_or_else(
            || value.clone(),
            |container| container.translate_env(key, value),
          )),
        )
      })
      .collect();
    ExprValue::Object(env_obj)
  }

  /// Build an `EvalContext` snapshot for the expression evaluator.
  pub fn eval_context(&self) -> EvalContext {
    let mut contexts = self.incoming_contexts.clone();
    if let Some(inputs) = self.scoped_inputs.get(&self.scope_path) {
      contexts.insert(
        "inputs".to_owned(),
        ExprValue::Object(string_map_to_obj(inputs)),
      );
    }
    contexts.insert("github".to_owned(), self.github_expression_context());
    contexts.insert("env".to_owned(), self.env_expression_context());
    contexts.insert("steps".to_owned(), self.steps_context());
    contexts.insert(
      "runner".to_owned(),
      ExprValue::Object(self.runner_context.clone()),
    );
    contexts.insert(
      "secrets".to_owned(),
      ExprValue::Object(string_map_to_obj(&self.secrets)),
    );
    contexts.insert(
      "vars".to_owned(),
      ExprValue::Object(string_map_to_obj(&self.vars)),
    );
    contexts.insert("job".to_owned(), ExprValue::Object(self.job_context()));

    EvalContext {
      contexts,
      job_status: self.scoped_job_status(),
      workspace: self.workspace.clone(),
    }
  }

  fn scoped_job_status(&self) -> JobStatus {
    if self.job_status() == JobStatus::Cancelled {
      return JobStatus::Cancelled;
    }
    self
      .scoped_status
      .get(&self.scope_path)
      .copied()
      .unwrap_or_else(|| self.job_status())
  }

  fn steps_context(&self) -> ExprValue {
    if self.scope_path.is_empty() {
      build_steps_context(&self.steps)
    } else {
      self
        .scoped_steps
        .get(&self.scope_path)
        .map_or_else(|| ExprValue::Object(HashMap::new()), build_steps_context)
    }
  }

  /// Build runtime job status and the owned container identity when present.
  /// Includes actual service container IDs and published host ports.
  fn job_context(&self) -> HashMap<String, ExprValue> {
    let mut job = HashMap::new();
    job.insert(
      "status".to_owned(),
      ExprValue::String(job_status_str(self.job_status()).to_owned()),
    );
    let container = self.container.as_ref().map_or(ExprValue::Null, |host| {
      ExprValue::Object(HashMap::from([
        ("id".to_owned(), ExprValue::String(host.id().to_owned())),
        (
          "network".to_owned(),
          ExprValue::String(host.network().to_owned()),
        ),
      ]))
    });
    job.insert("container".to_owned(), container);
    job.insert(
      "services".to_owned(),
      ExprValue::Object(self.services.as_deref().map_or_else(
        HashMap::new,
        crate::docker::services::ServiceContainers::context,
      )),
    );
    job
  }
}
