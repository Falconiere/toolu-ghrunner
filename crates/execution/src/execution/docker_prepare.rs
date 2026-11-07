//! Prepare remote Docker action images and report setup rows before workflow steps.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use shared::{ActionStep, Conclusion, RunnerError, RunnerEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::action_exec::build_uses_ref;
use super::action_support::{emit_log, read_manifest, resolve_action_dir};
use super::actions::manifest::{ActionDefinition, RunsUsing};
use super::actions::prefetch::ActionFetcher;
use super::actions::resolver::{ActionRef, ActionRefKind, parse_action_ref};
use super::context::ExecutionContext;
use super::depth_tracker::MAX_COMPOSITE_DEPTH;
use crate::docker::action_container::ActionContainer;

struct BuildAction {
  image: String,
  action_dir: PathBuf,
  cache_key: String,
}

struct BuildGroup {
  repository: String,
  actions: Vec<BuildAction>,
}

/// Inputs shared while discovering remote action manifests.
pub(super) struct Discovery<'a> {
  pub(super) fetcher: &'a ActionFetcher,
  pub(super) client: &'a reqwest::Client,
  pub(super) data_dir: &'a Path,
}

#[derive(Default)]
struct ImagePlan {
  pulls: Vec<String>,
  pull_keys: HashSet<String>,
  builds: Vec<BuildGroup>,
  build_keys: HashSet<String>,
}

#[derive(Clone, Copy)]
enum SetupTask<'a> {
  Pull(&'a str),
  Build(&'a BuildGroup),
}

/// Key shared by the prepass and step-time Docker dispatch.
pub(super) fn image_key(image: &str, cache_key: Option<&str>) -> Option<String> {
  if let Some(registry) = image.strip_prefix("docker://") {
    return Some(format!("pull:{}", registry.to_ascii_lowercase()));
  }
  cache_key.map(|key| format!("build:{key}"))
}

/// Resolve remote manifests and prepare their Docker images before user steps.
pub(super) async fn prepare(
  steps: &[ActionStep],
  source: Discovery<'_>,
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  cancel: &CancellationToken,
) -> Result<(), RunnerError> {
  let plan = discover(steps, &source, cancel).await?;
  let count = plan.pulls.len().saturating_add(plan.builds.len());
  if count == 0 {
    return Ok(());
  }
  if !cfg!(target_os = "linux") {
    emit_log(
      events,
      shared::SETUP_STEP_ID,
      "Container action is only supported on Linux, skip pull and build docker images.",
    )
    .await;
    return Ok(());
  }
  run_plan(&plan, ctx, events, cancel).await
}

async fn run_plan(
  plan: &ImagePlan,
  ctx: &mut ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  cancel: &CancellationToken,
) -> Result<(), RunnerError> {
  let count = plan.pulls.len().saturating_add(plan.builds.len());
  ctx.docker_setup_count = u32::try_from(count).unwrap_or(u32::MAX);
  for (index, image) in plan.pulls.iter().enumerate() {
    let prepared = run_task(SetupTask::Pull(image), index, ctx, events, cancel).await?;
    ctx.prepared_docker_images.extend(prepared);
  }
  for (index, group) in plan.builds.iter().enumerate() {
    let prepared = run_task(
      SetupTask::Build(group),
      plan.pulls.len().saturating_add(index),
      ctx,
      events,
      cancel,
    )
    .await?;
    ctx.prepared_docker_images.extend(prepared);
  }
  Ok(())
}

async fn discover(
  steps: &[ActionStep],
  source: &Discovery<'_>,
  cancel: &CancellationToken,
) -> Result<ImagePlan, RunnerError> {
  let mut plan = ImagePlan::default();
  let mut queue: VecDeque<_> = steps
    .iter()
    .filter(|step| !step.is_run_step())
    .map(|step| (build_uses_ref(&step.reference), 0))
    .collect();
  while let Some((uses, depth)) = queue.pop_front() {
    if cancel.is_cancelled() {
      return Err(RunnerError::Cancelled);
    }
    if depth > MAX_COMPOSITE_DEPTH {
      return Err(RunnerError::StepExecution(format!(
        "composite action depth limit exceeded (max {MAX_COMPOSITE_DEPTH})"
      )));
    }
    if let Some(image) = uses.strip_prefix("docker://") {
      plan.add_pull(image)?;
      continue;
    }
    discover_remote(&uses, depth, &mut plan, &mut queue, source).await?;
  }
  Ok(plan)
}

async fn discover_remote(
  uses: &str,
  depth: u32,
  plan: &mut ImagePlan,
  queue: &mut VecDeque<(String, u32)>,
  source: &Discovery<'_>,
) -> Result<(), RunnerError> {
  let Some((action_ref, action_dir, manifest)) = load_remote(uses, source).await else {
    return Ok(());
  };
  match manifest.runs.using {
    RunsUsing::Node { .. } => {},
    RunsUsing::Composite => {
      queue.extend(
        manifest
          .runs
          .steps
          .into_iter()
          .filter_map(|step| step.uses.map(|uses| (uses, depth.saturating_add(1)))),
      );
    },
    RunsUsing::Docker => {
      let Some(image) = manifest.runs.image.as_deref() else {
        return Ok(());
      };
      if let Some(registry) = image.strip_prefix("docker://") {
        plan.add_pull(registry)?;
      } else {
        plan.add_build(
          format!("{}/{}", action_ref.owner, action_ref.repo),
          image,
          action_dir,
        );
      }
    },
  }
  Ok(())
}

/// Unknown action types keep their existing step-time resolution behavior.
async fn load_remote(
  uses: &str,
  source: &Discovery<'_>,
) -> Option<(ActionRef, PathBuf, ActionDefinition)> {
  let action_ref = parse_action_ref(uses).ok()?;
  if action_ref.kind == ActionRefKind::Local {
    return None;
  }
  let cache_dir = source
    .fetcher
    .ensure_action(source.client, &action_ref, source.data_dir)
    .await
    .ok()?;
  let action_dir = resolve_action_dir(&cache_dir, action_ref.subpath.as_ref());
  let manifest = read_manifest(&action_dir).ok()?;
  Some((action_ref, action_dir, manifest))
}

impl ImagePlan {
  fn add_pull(&mut self, image: &str) -> Result<(), RunnerError> {
    if image.trim().is_empty() {
      return Err(RunnerError::ActionManifest(
        "Docker action registry image is empty".to_owned(),
      ));
    }
    if self.pull_keys.insert(image.to_ascii_lowercase()) {
      self.pulls.push(image.to_owned());
    }
    Ok(())
  }

  fn add_build(&mut self, repository: String, image: &str, action_dir: PathBuf) {
    let cache_key = action_dir.to_string_lossy().into_owned();
    if !self.build_keys.insert(cache_key.clone()) {
      return;
    }
    let action = BuildAction {
      image: image.to_owned(),
      action_dir,
      cache_key,
    };
    if let Some(group) = self
      .builds
      .iter_mut()
      .find(|group| group.repository.eq_ignore_ascii_case(&repository))
    {
      group.actions.push(action);
    } else {
      self.builds.push(BuildGroup {
        repository,
        actions: vec![action],
      });
    }
  }
}

async fn run_task(
  task: SetupTask<'_>,
  index: usize,
  ctx: &ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
  cancel: &CancellationToken,
) -> Result<Vec<(String, String)>, RunnerError> {
  let id = uuid::Uuid::new_v4().to_string();
  start_task(task, index, &id, events).await;
  let result = prepare_task(task, ctx, cancel).await;
  if let Err(error) = &result
    && !matches!(error, RunnerError::Cancelled)
  {
    emit_log(events, &id, &format!("##[error]{error}")).await;
  }
  let conclusion = match &result {
    Ok(_) => Conclusion::Success,
    Err(RunnerError::Cancelled) => Conclusion::Cancelled,
    Err(_) => Conclusion::Failure,
  };
  send(
    events,
    RunnerEvent::StepCompleted {
      step_id: id,
      conclusion,
      outputs: std::collections::HashMap::new(),
    },
  )
  .await;
  result
}

async fn start_task(
  task: SetupTask<'_>,
  index: usize,
  id: &str,
  events: &mpsc::Sender<RunnerEvent>,
) {
  let (name, action) = match task {
    SetupTask::Pull(image) => (format!("Pull {image}"), "pull_action_image"),
    SetupTask::Build(group) => (format!("Build {}", group.repository), "build_action_image"),
  };
  let number = u32::try_from(index).unwrap_or(u32::MAX).saturating_add(2);
  send(
    events,
    RunnerEvent::StepStarted {
      step_id: id.to_owned(),
      step_name: name.clone(),
      step_number: number,
    },
  )
  .await;
  send(
    events,
    RunnerEvent::StepMetadata {
      step_id: id.to_owned(),
      kind: "runner".to_owned(),
      action: Some(action.to_owned()),
      git_ref: None,
    },
  )
  .await;
  emit_log(events, id, &name).await;
}

async fn prepare_task(
  task: SetupTask<'_>,
  ctx: &ExecutionContext,
  cancel: &CancellationToken,
) -> Result<Vec<(String, String)>, RunnerError> {
  let runtime = ActionContainer::connect(Arc::clone(ctx.masker())).await?;
  match task {
    SetupTask::Pull(image) => {
      let full = format!("docker://{image}");
      let tag = runtime
        .prepare_image(&full, Path::new(""), None, cancel)
        .await?;
      let key = image_key(&full, None)
        .ok_or_else(|| RunnerError::Docker("missing registry key".to_owned()))?;
      Ok(vec![(key, tag)])
    },
    SetupTask::Build(group) => {
      let mut prepared = Vec::new();
      for action in &group.actions {
        let tag = runtime
          .prepare_image(
            &action.image,
            &action.action_dir,
            Some(&action.cache_key),
            cancel,
          )
          .await?;
        let key = image_key(&action.image, Some(&action.cache_key))
          .ok_or_else(|| RunnerError::Docker("missing action key".to_owned()))?;
        prepared.push((key, tag));
      }
      Ok(prepared)
    },
  }
}

async fn send(events: &mpsc::Sender<RunnerEvent>, event: RunnerEvent) {
  if events.send(event).await.is_err() {
    tracing::warn!("Docker setup event receiver closed");
  }
}

#[cfg(test)]
#[path = "tests/docker_prepare.rs"]
mod tests;
