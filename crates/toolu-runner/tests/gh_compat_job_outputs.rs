//! AC-8 (E1, S11) — job-level execution glue: job `outputs:` evaluation that
//! feeds `JobCompleted.outputs` (→ `needs.<job>.outputs.*`), `defaults.run`
//! shell/working-directory fallback for run-steps, and the self-hosted
//! `ACTIONS_RUNNER_HOOK_JOB_STARTED` / `_COMPLETED` hooks.
//!
//! Real-data only: steps run through the live step loop
//! (`execution::steps_runner::run_steps`) and hooks through the real hook
//! runner (`execution::job_hooks::run_job_hook`) — no mocks. The committed
//! `job_message.json` fixture is loaded to confirm it deserializes into the
//! same `AgentJobRequestMessage` shape the engine consumes.
//!
//! Asserts:
//!   1. A job `outputs: { out1: ${{ steps.s1.outputs.k }} }` where `s1` writes
//!      `k=v` to `$GITHUB_OUTPUT` resolves to `out1 == v`.
//!   2. `defaults.run.working-directory: sub` applies to a run-step with no
//!      `working-directory:` (cwd ends in `/sub`); a step that sets its own
//!      `working-directory` overrides the default.
//!   3. `ACTIONS_RUNNER_HOOK_JOB_STARTED` runs before steps (marker written),
//!      and a failing started-hook is reported as `Failure`.
//!   4. `ACTIONS_RUNNER_HOOK_JOB_COMPLETED` runs (marker written), best-effort.

use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, Mutex};

use shared::SecretMasker;
use shared::{
  ActionStep, ActionStepDefinitionReference, AgentJobRequestMessage, Conclusion, DictEntry,
  RunnerConfig, RunnerEvent, TemplateToken,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use toolu_runner::execution::context::ExecutionContext;
use toolu_runner::execution::job_hooks::{JobHookStage, run_job_hook};
use toolu_runner::execution::job_spec::{JobSpec, evaluate_job_outputs};
use toolu_runner::execution::steps_runner::{JobRun, run_steps};
use toolu_runner::execution::workflow::types::{RunDefaults, WorkflowDefaults};

const JOB_MESSAGE: &str = include_str!("fixtures/job_message.json");

type TestResult<T> = Result<T, Box<dyn Error>>;

/// Confirm the committed fixture deserializes into the engine's message type.
fn fixture_job() -> TestResult<AgentJobRequestMessage> {
  Ok(serde_json::from_str(JOB_MESSAGE)?)
}

/// A run-step `with:` input as a literal key/value pair.
fn lit_entry(key: &str, value: &str) -> DictEntry<TemplateToken> {
  DictEntry {
    key: TemplateToken {
      token_type: 0,
      lit: Some(key.to_owned()),
      ..TemplateToken::default()
    },
    value: TemplateToken {
      token_type: 0,
      lit: Some(value.to_owned()),
      ..TemplateToken::default()
    },
  }
}

/// Build a `run:` step. `shell`/`working_dir` are omitted from the inputs map
/// when `None`, so the job/workflow `defaults.run` fallback is exercised.
fn script_step(id: &str, body: &str, shell: Option<&str>, working_dir: Option<&str>) -> ActionStep {
  let mut entries = vec![lit_entry("script", body)];
  if let Some(s) = shell {
    entries.push(lit_entry("shell", s));
  }
  if let Some(wd) = working_dir {
    entries.push(lit_entry("workingDirectory", wd));
  }
  ActionStep {
    id: id.to_owned(),
    step_type: Some("script".to_owned()),
    display_name_token: None,
    context_name: Some(id.to_owned()),
    condition: None,
    continue_on_error: None,
    timeout_in_minutes: None,
    reference: ActionStepDefinitionReference::script(),
    inputs: TemplateToken {
      token_type: 2,
      d: Some(entries),
      ..TemplateToken::default()
    },
    environment: None,
  }
}

/// Drive `steps` through the real step loop with `spec`, returning the final
/// context. `workspace_setup` runs before the steps.
async fn run_steps_collect(
  steps: Vec<ActionStep>,
  spec: &JobSpec,
  workspace_setup: impl FnOnce(&std::path::Path) -> std::io::Result<()>,
) -> TestResult<ExecutionContext> {
  if fixture_job()?.job_id.is_empty() {
    return Err("fixture job_id missing".into());
  }

  let dir = tempfile::tempdir()?;
  let workspace = dir.path().join("work");
  std::fs::create_dir_all(&workspace)?;
  workspace_setup(&workspace)?;
  let config = RunnerConfig {
    data_dir: dir.path().join("data"),
    workspace_root: workspace.clone(),
    cgroup_path: None,
    services_mode: shared::ServicesMode::default(),
    ..RunnerConfig::default()
  };
  std::fs::create_dir_all(&config.data_dir)?;

  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let mut ctx = ExecutionContext::with_masker(Arc::clone(&masker));

  let (tx, mut rx) = mpsc::channel::<RunnerEvent>(1024);
  let collector = tokio::spawn(async move {
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
      events.push(event);
    }
    events
  });

  run_steps(
    &steps,
    &mut ctx,
    &tx,
    CancellationToken::new(),
    &JobRun {
      workspace: &workspace,
      config: &config,
      spec,
      shadow: None,
    },
  )
  .await?;
  drop(tx);
  collector.await?;
  Ok(ctx)
}

/// Build a `JobSpec` from a real parsed-workflow outputs map + defaults.
fn spec_with(
  outputs: &[(&str, &str)],
  wf_defaults: Option<&WorkflowDefaults>,
  job_defaults: Option<&WorkflowDefaults>,
) -> JobSpec {
  let outputs: HashMap<String, String> = outputs
    .iter()
    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
    .collect();
  JobSpec::from_workflow(outputs, wf_defaults, job_defaults)
}

/// Defaults carrying a `run.working-directory` value.
fn defaults_wd(wd: &str) -> WorkflowDefaults {
  WorkflowDefaults {
    run: Some(RunDefaults {
      shell: None,
      working_directory: Some(wd.to_owned()),
    }),
  }
}

// ---- AC-8: job outputs feed JobCompleted.outputs ---------------------------

#[tokio::test]
async fn job_outputs_resolve_from_step_github_output() -> TestResult<()> {
  // s1 writes `k=v` to $GITHUB_OUTPUT; the job output `out1` maps to it.
  let steps = vec![script_step(
    "s1",
    "echo \"k=hello-output\" >> \"$GITHUB_OUTPUT\"",
    Some("bash"),
    None,
  )];
  let spec = spec_with(&[("out1", "${{ steps.s1.outputs.k }}")], None, None);
  let ctx = run_steps_collect(steps, &spec, |_| Ok(())).await?;

  // This is the exact map run_job places into JobCompleted.outputs.
  let outputs = evaluate_job_outputs(&spec, &ctx)?;
  assert_eq!(
    outputs.get("out1").map(String::as_str),
    Some("hello-output"),
    "job output out1 must resolve to the step's $GITHUB_OUTPUT value"
  );
  Ok(())
}

#[tokio::test]
async fn job_outputs_empty_spec_yields_empty_map() -> TestResult<()> {
  let steps = vec![script_step("s1", "echo hi", Some("bash"), None)];
  let spec = JobSpec::default();
  let ctx = run_steps_collect(steps, &spec, |_| Ok(())).await?;
  let outputs = evaluate_job_outputs(&spec, &ctx)?;
  assert!(
    outputs.is_empty(),
    "no job outputs => empty JobCompleted.outputs"
  );
  Ok(())
}

// ---- AC-8: defaults.run working-directory ----------------------------------

#[tokio::test]
async fn defaults_run_working_directory_applies_to_bare_step() -> TestResult<()> {
  // No `working-directory:` on the step => defaults.run wins; pwd ends in /sub.
  let steps = vec![script_step(
    "s1",
    "echo \"k=$(pwd)\" >> \"$GITHUB_OUTPUT\"",
    Some("bash"),
    None,
  )];
  let job = defaults_wd("sub");
  let spec = spec_with(&[], None, Some(&job));

  let ctx = run_steps_collect(steps, &spec, |ws| std::fs::create_dir_all(ws.join("sub"))).await?;

  let pwd = ctx.step_outputs("s1").get("k").cloned().unwrap_or_default();
  assert!(
    pwd.ends_with("/sub"),
    "default working-directory `sub` must apply: pwd was {pwd}"
  );
  Ok(())
}

#[tokio::test]
async fn step_working_directory_overrides_default() -> TestResult<()> {
  // The step sets its own working-directory `own`, overriding default `sub`.
  let steps = vec![script_step(
    "s1",
    "echo \"k=$(pwd)\" >> \"$GITHUB_OUTPUT\"",
    Some("bash"),
    Some("own"),
  )];
  let job = defaults_wd("sub");
  let spec = spec_with(&[], None, Some(&job));

  let ctx = run_steps_collect(steps, &spec, |ws| {
    std::fs::create_dir_all(ws.join("sub"))?;
    std::fs::create_dir_all(ws.join("own"))
  })
  .await?;

  let pwd = ctx.step_outputs("s1").get("k").cloned().unwrap_or_default();
  assert!(
    pwd.ends_with("/own"),
    "step working-directory must override the default: pwd was {pwd}"
  );
  Ok(())
}

#[tokio::test]
async fn job_defaults_override_workflow_defaults() -> TestResult<()> {
  // Workflow default `wf`, job default `job` => job wins.
  let steps = vec![script_step(
    "s1",
    "echo \"k=$(pwd)\" >> \"$GITHUB_OUTPUT\"",
    Some("bash"),
    None,
  )];
  let wf = defaults_wd("wf");
  let job = defaults_wd("job");
  let spec = spec_with(&[], Some(&wf), Some(&job));

  let ctx = run_steps_collect(steps, &spec, |ws| {
    std::fs::create_dir_all(ws.join("wf"))?;
    std::fs::create_dir_all(ws.join("job"))
  })
  .await?;

  let pwd = ctx.step_outputs("s1").get("k").cloned().unwrap_or_default();
  assert!(
    pwd.ends_with("/job"),
    "job defaults.run must override workflow defaults.run: pwd was {pwd}"
  );
  Ok(())
}

// ---- AC-8: job hooks -------------------------------------------------------

/// Build a context whose env carries the hook env var, plus a temp workspace.
fn hook_ctx(env_var: &str, script_path: &str) -> ExecutionContext {
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let mut ctx = ExecutionContext::with_masker(masker);
  ctx.set_env(env_var, script_path);
  ctx
}

/// Drain the event channel returned by a hook run (events are not asserted on
/// here; the marker file is the ground truth).
async fn run_hook(
  stage: JobHookStage,
  ctx: &ExecutionContext,
  workspace: &std::path::Path,
) -> TestResult<Option<Conclusion>> {
  let (tx, mut rx) = mpsc::channel::<RunnerEvent>(1024);
  let drainer = tokio::spawn(async move { while rx.recv().await.is_some() {} });
  let cancel = CancellationToken::new();
  let conclusion = run_job_hook(stage, ctx, &tx, workspace, &cancel).await?;
  drop(tx);
  drainer.await?;
  Ok(conclusion)
}

#[tokio::test]
async fn job_started_hook_runs_and_succeeds() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let workspace = dir.path().join("work");
  std::fs::create_dir_all(&workspace)?;
  let marker = dir.path().join("started.marker");
  let hook = dir.path().join("started.sh");
  std::fs::write(
    &hook,
    format!("#!/usr/bin/env bash\necho ok > '{}'\n", marker.display()),
  )?;

  let ctx = hook_ctx("ACTIONS_RUNNER_HOOK_JOB_STARTED", &hook.to_string_lossy());
  let conclusion = run_hook(JobHookStage::Started, &ctx, &workspace).await?;

  assert_eq!(conclusion, Some(Conclusion::Success));
  assert!(
    marker.exists(),
    "job-started hook must run and write its marker"
  );
  Ok(())
}

#[tokio::test]
async fn job_started_hook_failure_is_reported() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let workspace = dir.path().join("work");
  std::fs::create_dir_all(&workspace)?;
  let hook = dir.path().join("fail.sh");
  std::fs::write(&hook, "#!/usr/bin/env bash\nexit 3\n")?;

  let ctx = hook_ctx("ACTIONS_RUNNER_HOOK_JOB_STARTED", &hook.to_string_lossy());
  let conclusion = run_hook(JobHookStage::Started, &ctx, &workspace).await?;

  assert_eq!(
    conclusion,
    Some(Conclusion::Failure),
    "a non-zero job-started hook must report Failure (the caller fails the job)"
  );
  Ok(())
}

#[tokio::test]
async fn job_completed_hook_runs() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let workspace = dir.path().join("work");
  std::fs::create_dir_all(&workspace)?;
  let marker = dir.path().join("completed.marker");
  let hook = dir.path().join("completed.sh");
  std::fs::write(
    &hook,
    format!("#!/usr/bin/env bash\necho done > '{}'\n", marker.display()),
  )?;

  let ctx = hook_ctx("ACTIONS_RUNNER_HOOK_JOB_COMPLETED", &hook.to_string_lossy());
  let conclusion = run_hook(JobHookStage::Completed, &ctx, &workspace).await?;

  assert_eq!(conclusion, Some(Conclusion::Success));
  assert!(
    marker.exists(),
    "job-completed hook must run and write its marker"
  );
  Ok(())
}

#[tokio::test]
async fn no_hook_env_is_a_noop() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let workspace = dir.path().join("work");
  std::fs::create_dir_all(&workspace)?;
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let ctx = ExecutionContext::with_masker(masker); // no hook env var set
  let conclusion = run_hook(JobHookStage::Started, &ctx, &workspace).await?;
  assert_eq!(conclusion, None, "unset hook env var => hook is skipped");
  Ok(())
}
