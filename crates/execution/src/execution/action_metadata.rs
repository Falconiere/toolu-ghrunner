//! Scoped GitHub action identity and runner-owned process environment exports.

use std::collections::HashMap;

use expressions::types::ExprValue;
use shared::{ActionStep, ActionStepDefinitionReference};

use super::action_exec::build_uses_ref;
use super::actions::resolver::{ActionRefKind, parse_action_ref};
use super::context::ExecutionContext;

const KEYS: [&str; 3] = ["action", "action_repository", "action_ref"];

/// Parent values retained while a step or embedded action runs.
pub(super) struct ActionMetadata([ExprValue; 3]);

impl ActionMetadata {
  /// Save the three runtime-owned github fields before entering a child.
  pub(super) fn capture(ctx: &ExecutionContext) -> Self {
    Self(KEYS.map(|key| {
      ctx
        .github_context_value(key)
        .cloned()
        .unwrap_or(ExprValue::Null)
    }))
  }

  /// Restore parent fields after the child returns, including errors.
  pub(super) fn restore(self, ctx: &mut ExecutionContext) {
    for (key, value) in KEYS.into_iter().zip(self.0) {
      ctx.set_github_context_value(key, value);
    }
  }
}

/// Use the acquired wire name; legacy local steps may only have contextName.
pub(super) fn set_name(ctx: &mut ExecutionContext, step: &ActionStep) {
  let name = step
    .name
    .as_deref()
    .filter(|name| !name.is_empty())
    .or_else(|| step.context_name.as_deref().filter(|name| !name.is_empty()))
    .unwrap_or_default();
  ctx.set_github_context("action", name);
}

/// Repository identity excludes the action subpath and is empty for local/image/run.
pub(super) fn set_reference(ctx: &mut ExecutionContext, reference: &ActionStepDefinitionReference) {
  clear_repository(ctx);
  if reference.ref_type.as_deref() == Some("script") {
    return;
  }
  if let Ok(action) = parse_action_ref(&build_uses_ref(reference))
    && action.kind == ActionRefKind::Remote
  {
    ctx.set_github_context(
      "action_repository",
      &format!("{}/{}", action.owner, action.repo),
    );
    ctx.set_github_context("action_ref", &action.git_ref);
  }
}

/// Script and local/image actions must not inherit the previous action's repository.
/// Upstream `SetGitHubContext` wraps null in `StringContextData`, whose `Value` and
/// `IString.GetString` normalize it to an empty string for expressions and env.
pub(super) fn clear_repository(ctx: &mut ExecutionContext) {
  ctx.set_github_context("action_repository", "");
  ctx.set_github_context("action_ref", "");
}

/// Export metadata after user overlays so stale values cannot reach a child.
pub(super) fn export(ctx: &ExecutionContext, env: &mut HashMap<String, String>) {
  for key in KEYS {
    env.insert(
      format!("GITHUB_{}", key.to_ascii_uppercase()),
      ctx.github_context(key).unwrap_or_default().to_owned(),
    );
  }
  env.insert("RUNNER_ENVIRONMENT".to_owned(), "self-hosted".to_owned());
}
