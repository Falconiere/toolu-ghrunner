//! A reported step's action identity, mirroring upstream
//! `Handler.PopulateActionTelemetry` plus the handler overrides
//! (`NodeScriptActionHandler`, `CompositeActionHandler`,
//! `ContainerActionHandler`, `ScriptHandler`) at actions/runner cab9d1c.

use shared::{ActionStepDefinitionReference, RunnerEvent};
use tokio::sync::mpsc;

use super::actions::manifest::RunsUsing;

/// Emit the step's `type` / `action_name` / `ref` once its handler is known.
pub(crate) async fn emit_step_metadata(
  events: &mpsc::Sender<RunnerEvent>,
  step_id: &str,
  kind: String,
  action: Option<String>,
  git_ref: Option<String>,
) {
  let event = RunnerEvent::StepMetadata {
    step_id: step_id.to_owned(),
    kind,
    action,
    git_ref,
  };
  if events.send(event).await.is_err() {
    tracing::warn!(step_id, "event channel closed; step metadata was dropped");
  }
}

/// Emit the metadata of a `uses:` step run by the `using` handler.
pub(crate) async fn emit_action_metadata(
  events: &mpsc::Sender<RunnerEvent>,
  step_id: &str,
  reference: &ActionStepDefinitionReference,
  using: &RunsUsing,
) {
  let (kind, action, git_ref) = action_metadata(reference, using);
  emit_step_metadata(events, step_id, kind, action, git_ref).await;
}

/// Upstream's telemetry for a `uses:` step: a `docker://` step is `DockerHub`
/// with its image; a repository action reports `owner/repo[/path]` and its ref
/// (a `self` action only its path) with the handler kind.
pub(crate) fn action_metadata(
  reference: &ActionStepDefinitionReference,
  using: &RunsUsing,
) -> (String, Option<String>, Option<String>) {
  if reference
    .ref_type
    .as_deref()
    .is_some_and(|kind| kind.eq_ignore_ascii_case("containerRegistry"))
  {
    let image = reference.image.as_deref().unwrap_or_default();
    let image = image.strip_prefix("docker://").unwrap_or(image);
    return ("DockerHub".to_owned(), Some(image.to_owned()), None);
  }
  let kind = match using {
    RunsUsing::Node { major } => format!("node{major}"),
    RunsUsing::Composite => "composite".to_owned(),
    // Upstream: `Action.Type == Repository ? "Dockerfile" : "DockerHub"`.
    RunsUsing::Docker => "Dockerfile".to_owned(),
  };
  if reference.repository_type.as_deref() == Some("self") {
    return (kind, reference.path.clone(), None);
  }
  let name = reference.name.clone().unwrap_or_default();
  let action = match reference.path.as_deref() {
    Some(path) if !path.is_empty() => format!("{name}/{path}"),
    _ => name,
  };
  (kind, Some(action), reference.git_ref.clone())
}

#[cfg(test)]
#[path = "tests/step_metadata.rs"]
mod tests;
