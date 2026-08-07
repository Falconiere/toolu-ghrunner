//! Action step types for job execution.

use serde::{Deserialize, Serialize};

use super::context_data::DictEntry;
use super::template_token::TemplateToken;

/// A single step in the job.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionStep {
  /// The step's id within the job.
  pub id: String,
  /// Wire field `type`; the step's handler kind (e.g. `"script"`), if reported.
  #[serde(default, rename = "type")]
  pub step_type: Option<String>,
  /// The step's `name:` as a template token, if specified.
  #[serde(default)]
  pub display_name_token: Option<TemplateToken>,
  /// The `steps.<context_name>` key this step's outputs are addressed by.
  #[serde(default)]
  pub context_name: Option<String>,
  /// The step's `if:` condition expression, if specified.
  #[serde(default)]
  pub condition: Option<String>,
  /// The step's `continue-on-error:` setting, if specified.
  #[serde(default)]
  pub continue_on_error: Option<bool>,
  /// The step's `timeout-minutes:` setting, if specified.
  #[serde(default)]
  pub timeout_in_minutes: Option<u32>,
  /// Reference to the action/script this step runs.
  pub reference: ActionStepDefinitionReference,
  /// The step's `with:` input values.
  #[serde(default)]
  pub inputs: TemplateToken,
  /// The step's `env:` values, if specified.
  #[serde(default)]
  pub environment: Option<TemplateToken>,
}

impl ActionStep {
  /// Set the continue-on-error flag.
  pub fn set_continue_on_error(&mut self, value: bool) {
    self.continue_on_error = Some(value);
  }

  /// Extract the script body from step inputs.
  pub fn script_body(&self) -> Option<String> {
    let map = self.inputs.to_map();
    map
      .get("script")
      .and_then(|t| t.to_string_value())
      .map(ToOwned::to_owned)
  }

  /// Extract the shell name from step inputs.
  pub fn shell_name(&self) -> Option<String> {
    let map = self.inputs.to_map();
    map
      .get("shell")
      .and_then(|t| t.to_string_value())
      .map(ToOwned::to_owned)
  }

  /// Returns true if this is a script (run:) step.
  pub fn is_run_step(&self) -> bool {
    self
      .reference
      .ref_type
      .as_deref()
      .is_some_and(|t| t == "script")
  }

  /// Returns the handler type from the definition reference (e.g., "node20", "docker", "script").
  pub fn runs_using(&self) -> Option<&str> {
    self.reference.ref_type.as_deref()
  }

  /// Extract a named input value from the step's inputs map.
  pub fn input(&self, key: &str) -> Option<String> {
    let map = self.inputs.to_map();
    map
      .get(key)
      .and_then(|t| t.to_string_value())
      .map(ToOwned::to_owned)
  }

  /// Build a minimal script step for testing.
  pub fn script(id: &str, script_body: &str, condition: &str) -> Self {
    let inputs = TemplateToken {
      token_type: 2,
      d: Some(vec![DictEntry {
        key: TemplateToken {
          token_type: 0,
          lit: Some("script".to_owned()),
          ..TemplateToken::default()
        },
        value: TemplateToken {
          token_type: 0,
          lit: Some(script_body.to_owned()),
          ..TemplateToken::default()
        },
      }]),
      ..TemplateToken::default()
    };

    Self {
      id: id.to_owned(),
      step_type: Some("script".to_owned()),
      display_name_token: None,
      context_name: Some(id.to_owned()),
      condition: if condition.is_empty() {
        None
      } else {
        Some(condition.to_owned())
      },
      continue_on_error: None,
      timeout_in_minutes: None,
      reference: ActionStepDefinitionReference::script(),
      inputs,
      environment: None,
    }
  }

  /// Build a minimal step with a specific `ref_type` (used for handler dispatch in tests).
  pub fn with_ref_type(id: &str, ref_type: &str) -> Self {
    Self {
      id: id.to_owned(),
      step_type: Some(ref_type.to_owned()),
      display_name_token: None,
      context_name: Some(id.to_owned()),
      condition: None,
      continue_on_error: None,
      timeout_in_minutes: None,
      reference: ActionStepDefinitionReference {
        ref_type: Some(ref_type.to_owned()),
        image: None,
        name: None,
        git_ref: None,
        repository_type: None,
        path: None,
      },
      inputs: TemplateToken::default(),
      environment: None,
    }
  }
}

/// Reference to the action/script definition for a step.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionStepDefinitionReference {
  /// Wire field `type`; the handler kind (e.g. `"script"`, `"node20"`, `"docker"`).
  #[serde(default, rename = "type")]
  pub ref_type: Option<String>,
  /// The Docker image reference, for `docker` steps.
  #[serde(default)]
  pub image: Option<String>,
  /// The action's `owner/repo` (or local path) name, for `uses:` steps.
  #[serde(default)]
  pub name: Option<String>,
  /// Wire field `ref`; the action's git ref (tag/branch/SHA), if specified.
  #[serde(default, rename = "ref")]
  pub git_ref: Option<String>,
  /// The repository type the action is sourced from (e.g. `"GitHub"`, `"Local"`).
  #[serde(default)]
  pub repository_type: Option<String>,
  /// The local path to a composite/JS action, for local `uses: ./...` steps.
  #[serde(default)]
  pub path: Option<String>,
}

impl ActionStepDefinitionReference {
  /// Build a script reference (for `run:` steps).
  pub fn script() -> Self {
    Self {
      ref_type: Some("script".to_owned()),
      image: None,
      name: None,
      git_ref: None,
      repository_type: None,
      path: None,
    }
  }
}
