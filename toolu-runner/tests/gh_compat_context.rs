//! AC-9 (E2, S12 + S13) — expression-context completeness.
//!
//! Drives the REAL context-assembly path
//! (`execution::job_runner::build_context`) from the committed
//! `tests/fixtures/job_message.json`, then evaluates `${{ ... }}` through the
//! same evaluator steps use (`ExecutionContext::evaluate_expression`) — no
//! mocks, no hand-rolled lookup. Asserts:
//!
//! S12 (runner.*/github.*):
//!   - `runner.os == "Linux"`, `runner.arch` == this host's arch,
//!     `runner.temp`/`runner.tool_cache` are real existing dirs.
//!   - `github.sha`/`github.repository`/`github.run_id` == the fixture values.
//!
//! S13 (vars/secrets/job/strategy/steps):
//!   - `vars.MY_VAR` resolves to the fixture's non-secret config variable.
//!   - `secrets.MY_SECRET` resolves to the fixture's secret AND that value is
//!     masked in a log line via the shared masker.
//!   - `job.status` resolves; `strategy.*` carries single-job defaults.
//!   - `steps.<id>.outcome`/`.conclusion` are distinct, and `.state` is exposed.

use std::error::Error;
use std::sync::{Arc, Mutex};

use shared::{AgentJobRequestMessage, Conclusion, RunnerConfig};
use toolu_runner::execution::context::ExecutionContext;
use toolu_runner::execution::job_runner::build_context;
use toolu_runner::execution::secret_masker::SecretMasker;

const JOB_MESSAGE: &str = include_str!("fixtures/job_message.json");

type TestResult<T> = Result<T, Box<dyn Error>>;

/// Deserialize the committed fixture into the engine's message type.
fn fixture_job() -> TestResult<AgentJobRequestMessage> {
  Ok(serde_json::from_str(JOB_MESSAGE)?)
}

/// Build the real `ExecutionContext` from the fixture + a temp-dir config,
/// returning the context and the shared masker for masking assertions.
fn build_ctx(
  data_dir: &std::path::Path,
) -> TestResult<(ExecutionContext, Arc<Mutex<SecretMasker>>)> {
  let msg = fixture_job()?;
  let config = RunnerConfig {
    data_dir: data_dir.to_path_buf(),
    workspace_root: data_dir.join("_work"),
    cgroup_path: None,
    services_mode: shared::ServicesMode::default(),
  };
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let ctx = build_context(&msg, &config, Arc::clone(&masker));
  Ok((ctx, masker))
}

/// Interpolate a `${{ ... }}` template through the real evaluator path that
/// steps use for `with:`/conditions, returning the resolved string.
fn eval(ctx: &ExecutionContext, expr: &str) -> TestResult<String> {
  Ok(ctx.interpolate_string(expr)?)
}

#[test]
fn runner_context_is_host_and_config_derived() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let (ctx, _masker) = build_ctx(dir.path())?;

  // os is fixed to the Linux target; arch matches the host mapping.
  assert_eq!(eval(&ctx, "${{ runner.os }}")?, "Linux");

  let expected_arch = match std::env::consts::ARCH {
    "x86_64" => "X64",
    "aarch64" => "ARM64",
    "arm" => "ARM",
    "x86" => "X86",
    other => other,
  };
  assert_eq!(eval(&ctx, "${{ runner.arch }}")?, expected_arch);

  // temp/tool_cache are real existing dirs under data_dir (Open Q6).
  let temp = eval(&ctx, "${{ runner.temp }}")?;
  let tool_cache = eval(&ctx, "${{ runner.tool_cache }}")?;
  assert!(
    std::path::Path::new(&temp).is_dir(),
    "runner.temp should be an existing dir, got {temp}"
  );
  assert!(
    std::path::Path::new(&tool_cache).is_dir(),
    "runner.tool_cache should be an existing dir, got {tool_cache}"
  );
  assert_eq!(temp, dir.path().join("_temp").to_string_lossy());
  assert_eq!(tool_cache, dir.path().join("_tool").to_string_lossy());

  // name comes from the message's runner dict (fixture: "GitHub Actions 1").
  assert_eq!(eval(&ctx, "${{ runner.name }}")?, "GitHub Actions 1");
  Ok(())
}

#[test]
fn github_context_matches_fixture_values() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let (ctx, _masker) = build_ctx(dir.path())?;

  assert_eq!(
    eval(&ctx, "${{ github.sha }}")?,
    "d6cd1e2bd19e03a81132a23b2025920577f84e37"
  );
  assert_eq!(
    eval(&ctx, "${{ github.repository }}")?,
    "octocat/hello-world"
  );
  assert_eq!(eval(&ctx, "${{ github.run_id }}")?, "9876543210");
  // A field the fixture carries beyond the three required ones.
  assert_eq!(eval(&ctx, "${{ github.repository_owner }}")?, "octocat");
  assert_eq!(eval(&ctx, "${{ github.ref_name }}")?, "main");
  Ok(())
}

#[test]
fn vars_context_resolves_config_variable() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let (ctx, _masker) = build_ctx(dir.path())?;

  assert_eq!(eval(&ctx, "${{ vars.MY_VAR }}")?, "repo-config-value");
  assert_eq!(eval(&ctx, "${{ vars.DEPLOY_ENV }}")?, "staging");
  Ok(())
}

#[test]
fn secrets_context_resolves_and_value_is_masked() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let (ctx, masker) = build_ctx(dir.path())?;

  // The user secret resolves through the real evaluator.
  let secret = eval(&ctx, "${{ secrets.MY_SECRET }}")?;
  assert_eq!(secret, "s3cr3t-token-value-XYZ");

  // The auto github token is NOT exposed in secrets.* (matches actions/runner),
  // but IS available as github.token.
  assert_eq!(
    eval(&ctx, "${{ secrets.GITHUB_TOKEN }}")?,
    "",
    "auto token must not surface in secrets.*"
  );
  assert_eq!(
    eval(&ctx, "${{ github.token }}")?,
    "ghs_EXAMPLEEXAMPLEEXAMPLEEXAMPLEEXAMPLE0000"
  );

  // The secret value is masked in a log line by the SAME shared masker that
  // build_context registered it with (and that the tracing sink uses).
  let line = format!("leaking the secret {secret} into a log line");
  let redacted = {
    let guard = masker
      .lock()
      .map_err(|err| format!("masker mutex poisoned: {err}"))?;
    guard.mask(&line)
  };
  assert!(
    !redacted.contains(&secret),
    "secret should be masked, got: {redacted}"
  );
  assert!(redacted.contains("***"), "masked output should contain ***");
  Ok(())
}

#[test]
fn job_and_strategy_contexts_resolve() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let (ctx, _masker) = build_ctx(dir.path())?;

  // job.status is real (success for a fresh context).
  assert_eq!(eval(&ctx, "${{ job.status }}")?, "success");

  // strategy.* carries single-job defaults for a non-matrix run.
  assert_eq!(eval(&ctx, "${{ strategy.job-index }}")?, "0");
  assert_eq!(eval(&ctx, "${{ strategy.job-total }}")?, "1");
  assert_eq!(eval(&ctx, "${{ strategy.fail-fast }}")?, "true");
  // max-parallel is null when the workflow does not pin it.
  assert_eq!(eval(&ctx, "${{ strategy.max-parallel }}")?, "");
  Ok(())
}

#[test]
fn steps_context_exposes_outcome_conclusion_and_state() -> TestResult<()> {
  let dir = tempfile::tempdir()?;
  let (mut ctx, _masker) = build_ctx(dir.path())?;

  // A step that failed under continue-on-error: outcome=failure, conclusion=success.
  ctx.set_step_outcome("build", Conclusion::Failure);
  ctx.set_step_conclusion("build", Conclusion::Success);
  ctx.set_step_output("build", "result", "ok");
  ctx.set_step_state("build", "saved", "value-123");

  assert_eq!(eval(&ctx, "${{ steps.build.outcome }}")?, "failure");
  assert_eq!(eval(&ctx, "${{ steps.build.conclusion }}")?, "success");
  assert_ne!(
    eval(&ctx, "${{ steps.build.outcome }}")?,
    eval(&ctx, "${{ steps.build.conclusion }}")?,
    "outcome and conclusion must be distinct under continue-on-error"
  );
  assert_eq!(eval(&ctx, "${{ steps.build.outputs.result }}")?, "ok");

  // .state is exposed in the steps context AND via the post-stage accessor.
  assert_eq!(eval(&ctx, "${{ steps.build.state.saved }}")?, "value-123");
  assert_eq!(
    ctx.step_state("build").get("saved").map(String::as_str),
    Some("value-123")
  );
  Ok(())
}
