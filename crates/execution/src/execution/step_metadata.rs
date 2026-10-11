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
/// (a `self` action only its path) with the handler kind. A reference missing
/// its image or name reports no action name (with a WARN) rather than `""`.
pub(crate) fn action_metadata(
  reference: &ActionStepDefinitionReference,
  using: &RunsUsing,
) -> (String, Option<String>, Option<String>) {
  if reference
    .ref_type
    .as_deref()
    .is_some_and(|kind| kind.eq_ignore_ascii_case("containerRegistry"))
  {
    let Some(image) = reference.image.as_deref() else {
      tracing::warn!("container registry step has no image; reporting no action name");
      return ("DockerHub".to_owned(), None, None);
    };
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
  let Some(name) = reference.name.clone() else {
    tracing::warn!("repository action has no name; reporting no action name");
    return (kind, None, reference.git_ref.clone());
  };
  let action = match reference.path.as_deref() {
    Some(path) if !path.is_empty() => format!("{name}/{path}"),
    _ => name,
  };
  (kind, Some(action), reference.git_ref.clone())
}

#[cfg(test)]
#[path = "tests/step_metadata.rs"]
mod tests;
