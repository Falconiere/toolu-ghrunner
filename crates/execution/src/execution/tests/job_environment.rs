//! Issue 69: captured-envelope transformations through real job execution.
//!
//! The #68 acquisition preserves incoming context types and step wire shapes.
//! Layers and scripts below are explicit probe transformations, not captures.
//! AC-1/69-S1: job/step values and expressions; AC-2/69-S2: scalar/context bytes;
//! AC-3/69-S3/S4: file commands, nested actions and successive job isolation;
//! AC-4: malformed setup fails before steps. Run: `cargo test -p execution job_environment`.

use std::error::Error;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use shared::{AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker};
use tokio_util::sync::CancellationToken;

type TestResult = Result<(), Box<dyn Error>>;
const CAPTURE: &str = include_str!("../../../tests/incoming_contexts_matrix_0.json");
const LAYERS: &str = include_str!("../../../tests/job_environment_layers.json");

fn literal(value: &str) -> serde_json::Value {
  serde_json::json!({"type":0,"lit":value})
}

fn mapping(values: &[(&str, serde_json::Value)]) -> serde_json::Value {
  serde_json::json!({"type":2,"map":values.iter().map(|(key,value)|
    serde_json::json!({"Key":literal(key),"Value":value})).collect::<Vec<_>>()})
}

fn set(value: &mut serde_json::Value, key: &str, item: serde_json::Value) -> TestResult {
  value
    .as_object_mut()
    .ok_or("expected object")?
    .insert(key.to_owned(), item);
  Ok(())
}

fn captured_variant(scripts: &[(&str, Option<&str>)]) -> Result<serde_json::Value, Box<dyn Error>> {
  let mut raw: serde_json::Value = serde_json::from_str(CAPTURE)?;
  set(
    &mut raw,
    "environmentVariables",
    serde_json::from_str(LAYERS)?,
  )?;
  // Explicit vars addition; the original acquisition has an empty vars mapping.
  set(
    raw.get_mut("contextData").ok_or("contextData")?,
    "vars",
    serde_json::json!({"t":2,"d":[{"k":"PROBE","v":"vars-value"}]}),
  )?;
  let template = raw.pointer("/steps/1").ok_or("captured script")?.clone();
  let mut steps = Vec::new();
  for (i, (script, env)) in scripts.iter().enumerate() {
    let mut step = template.clone();
    set(
      &mut step,
      "id",
      serde_json::json!(uuid::Uuid::new_v4().to_string()),
    )?;
    set(
      &mut step,
      "contextName",
      serde_json::json!(format!("probe_{i}")),
    )?;
    set(
      &mut step,
      "inputs",
      mapping(&[("script", literal(script)), ("shell", literal("bash"))]),
    )?;
    if let Some(value) = env {
      set(
        &mut step,
        "environment",
        mapping(&[("SHARED", literal(value))]),
      )?;
    }
    steps.push(step);
  }
  set(&mut raw, "steps", serde_json::Value::Array(steps))?;
  Ok(raw)
}

fn config(root: &Path) -> RunnerConfig {
  RunnerConfig {
    data_dir: root.join("data"),
    workspace_root: root.join("work"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  }
}

struct Observed {
  conclusion: Option<Conclusion>,
  starts: usize,
  logs: Vec<String>,
}

fn replay(raw: serde_json::Value, cfg: RunnerConfig) -> Result<Observed, Box<dyn Error>> {
  let msg: AgentJobRequestMessage = serde_json::from_value(raw)?;
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let runner = crate::Runner::new(cfg, Arc::clone(&masker));
  let runtime = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()?;
  runtime.block_on(async {
    let cancel = CancellationToken::new();
    let mut events = runner.execute_job(msg, cancel.clone());
    let result = tokio::time::timeout(Duration::from_secs(60), async {
      let mut observed = Observed {
        conclusion: None,
        starts: 0,
        logs: Vec::new(),
      };
      while let Some(event) = events.recv().await {
        match event {
          RunnerEvent::Log { line, .. } => observed.logs.push(
            masker
              .lock()
              .map_err(|e| e.to_string())?
              .mask(&line)
              .into_owned(),
          ),
          RunnerEvent::StepStarted { .. } => observed.starts += 1,
          RunnerEvent::JobCompleted { conclusion, .. } => observed.conclusion = Some(conclusion),
          RunnerEvent::JobStarted { .. }
          | RunnerEvent::StepCompleted { .. }
          | RunnerEvent::StepSkipped { .. }
          | RunnerEvent::LogGroup { .. }
          | RunnerEvent::Annotation { .. } => {},
        }
      }
      Ok::<_, Box<dyn Error>>(observed)
    })
    .await;
    cancel.cancel();
    result?
  })
}

#[test]
fn job_environment_layers_and_step_expressions_reach_real_shell() -> TestResult {
  let dir = tempfile::tempdir()?;
  let raw = captured_variant(&[(
    r#"
test "$SHARED" = step
test '${{ env.SHARED }}' = step
test "$PRIOR" = workflow
test "$SAME_LAYER" = workflow
test "$WORKFLOW_ONLY" = workflow-only
test "$GITHUB_VALUE" = Falconiere/toolu-ghrunner
test "$VARS_VALUE" = vars-value
test "$INPUT_VALUE" = world
test "$MATRIX_VALUE" = alpha
test "$NEEDS_VALUE" = artifact-68
test "$STRATEGY_VALUE" = 2
test "$EMPTY" = ''
test "$CI" = ''
test "$FROM_MISSING" = ''
test "$MULTILINE" = $'first\nsecond\n'
printf '%s' "$LITERAL" > literal.txt
test -n "$SECRET_VALUE"
printf 'secret-probe:%s\n' "$SECRET_VALUE"
printf ok > passed.txt
"#,
    Some("step"),
  )])?;
  let secret = raw
    .pointer("/variables/github_token/value")
    .ok_or("captured secret")?
    .as_str()
    .ok_or("missing captured secret")?
    .to_owned();
  let cfg = config(dir.path());
  let workspace = cfg
    .workspace_root
    .join(raw.get("jobId").ok_or("jobId")?.as_str().ok_or("jobId")?);
  let observed = replay(raw, cfg)?;
  assert_eq!(
    observed.conclusion,
    Some(Conclusion::Success),
    "{:?}",
    observed.logs
  );
  assert_eq!(std::fs::read_to_string(workspace.join("passed.txt"))?, "ok");
  assert_eq!(
    std::fs::read_to_string(workspace.join("literal.txt"))?,
    "${{ matrix.tag }}"
  );
  assert!(!observed.logs.join("\n").contains(&secret));
  assert!(observed.logs.iter().any(|line| line == "secret-probe:***"));
  Ok(())
}

#[test]
fn job_environment_file_updates_and_successive_jobs_are_isolated() -> TestResult {
  let dir = tempfile::tempdir()?;
  let raw = captured_variant(&[
    (
      "test \"$SHARED\" = job\nprintf 'SHARED=file\\n' >> \"$GITHUB_ENV\"\ntest \"$SHARED\" = job",
      None,
    ),
    (
      "test \"$SHARED\" = override\ntest '${{ env.SHARED }}' = override",
      Some("override"),
    ),
    (
      "test \"$SHARED\" = file\ntest '${{ env.SHARED }}' = file",
      None,
    ),
  ])?;
  let first = replay(raw, config(dir.path()))?;
  assert_eq!(
    first.conclusion,
    Some(Conclusion::Success),
    "{:?}",
    first.logs
  );
  assert_eq!(first.starts, 3);
  let mut next = captured_variant(&[(
    "test -z \"${SHARED+x}\"\ntest '${{ env.SHARED }}' = ''",
    None,
  )])?;
  set(&mut next, "environmentVariables", serde_json::json!([]))?;
  let second = replay(next, config(dir.path()))?;
  assert_eq!(
    second.conclusion,
    Some(Conclusion::Success),
    "{:?}",
    second.logs
  );
  Ok(())
}

#[test]
fn job_environment_invalid_layers_fail_before_any_step() -> TestResult {
  for bad in [
    literal("not a mapping"),
    serde_json::json!("malformed layer"),
    mapping(&[(
      "BAD",
      serde_json::json!({"type":0,"lit":["sensitive-wire-probe-69"]}),
    )]),
    mapping(&[("BAD", serde_json::json!({"type":99,"future":"payload"}))]),
    mapping(&[("BAD", serde_json::json!({"type":1,"seq":[]}))]),
    mapping(&[("BAD", serde_json::json!({"type":3,"expr":"inputs["}))]),
  ] {
    let dir = tempfile::tempdir()?;
    let mut raw = captured_variant(&[("exit 0", None)])?;
    set(&mut raw, "environmentVariables", serde_json::json!([bad]))?;
    let observed = replay(raw, config(dir.path()))?;
    assert_eq!(
      observed.conclusion,
      Some(Conclusion::Failure),
      "{:?}",
      observed.logs
    );
    assert_eq!(observed.starts, 0);
    let logs = observed.logs.join("\n");
    assert!(logs.contains("environmentVariables layer 0:"));
    assert!(!logs.contains("sensitive-wire-probe-69"));
  }
  Ok(())
}

#[test]
fn job_environment_absent_empty_and_null_layers_are_noops() -> TestResult {
  for layers in [
    None,
    Some(serde_json::json!([])),
    Some(serde_json::json!([
      {"type":7}, {"type":2,"map":[]}
    ])),
  ] {
    let dir = tempfile::tempdir()?;
    let mut raw = captured_variant(&[("test -z \"${SHARED+x}\"", None)])?;
    raw
      .as_object_mut()
      .ok_or("message object")?
      .remove("environmentVariables");
    if let Some(layers) = layers {
      set(&mut raw, "environmentVariables", layers)?;
    }
    let observed = replay(raw, config(dir.path()))?;
    assert_eq!(
      observed.conclusion,
      Some(Conclusion::Success),
      "{:?}",
      observed.logs
    );
    assert_eq!(observed.starts, 1);
  }
  Ok(())
}

#[test]
fn job_environment_scalar_coercion_and_expression_keys() -> TestResult {
  let dir = tempfile::tempdir()?;
  let mut raw = captured_variant(&[(
    r#"
test "$BOOL" = true
test "$NUMBER" = 42
test "$NULL" = ''
test "$DYNAMIC" = world
test "$LOWER" = upper
test "$lower" = lower
printf '%s' "$EXPRESSION_LITERAL" > expression-literal.txt
"#,
    None,
  )])?;
  let mut layer = mapping(&[
    ("BOOL", serde_json::json!({"type":5,"bool":true})),
    ("NUMBER", serde_json::json!({"type":6,"num":42})),
    ("NULL", serde_json::json!({"type":7})),
    ("LOWER", literal("upper")),
    ("lower", literal("lower")),
    (
      "EXPRESSION_LITERAL",
      serde_json::json!({"type":3,"expr":"env.LITERAL"}),
    ),
  ]);
  layer
    .get_mut("map")
    .ok_or("map")?
    .as_array_mut()
    .ok_or("entries")?
    .push(serde_json::json!({
      "Key":{"type":3,"expr":"'DYNAMIC'"},"Value":{"type":3,"expr":"inputs.who"}
    }));
  raw
    .get_mut("environmentVariables")
    .ok_or("layers")?
    .as_array_mut()
    .ok_or("layers")?
    .push(layer);
  let cfg = config(dir.path());
  let workspace = cfg
    .workspace_root
    .join(raw.get("jobId").ok_or("jobId")?.as_str().ok_or("job id")?);
  let observed = replay(raw, cfg)?;
  assert_eq!(
    observed.conclusion,
    Some(Conclusion::Success),
    "{:?}",
    observed.logs
  );
  assert_eq!(
    std::fs::read_to_string(workspace.join("expression-literal.txt"))?,
    "${{ matrix.tag }}"
  );
  Ok(())
}

#[test]
fn job_environment_malformed_token_details_do_not_leak_secrets() -> TestResult {
  let dir = tempfile::tempdir()?;
  let mut raw = captured_variant(&[("exit 0", None)])?;
  // A malformed expression containing sensitive literal data must not be echoed.
  let diagnostic_probe = "sensitive-expression-probe-69";
  set(
    &mut raw,
    "environmentVariables",
    serde_json::json!([mapping(&[(
      "BAD",
      serde_json::json!({"type":3,"expr":format!("'{diagnostic_probe}' + (")})
    )])]),
  )?;
  let observed = replay(raw, config(dir.path()))?;
  assert_eq!(observed.conclusion, Some(Conclusion::Failure));
  assert_eq!(observed.starts, 0);
  let logs = observed.logs.join("\n");
  assert!(logs.contains("environmentVariables layer 0: expression evaluation failed"));
  assert!(!logs.contains(diagnostic_probe));
  Ok(())
}

#[test]
fn job_environment_upstream_omitted_default_payloads_keep_their_values() -> TestResult {
  // Pinned upstream MappingToken/StringToken.OnSerializing removes empty
  // payloads; BooleanToken/NumberToken use EmitDefaultValue=false as well.
  let dir = tempfile::tempdir()?;
  let mut raw = captured_variant(&[(
    "test \"$EMPTY\" = ''\ntest \"$BOOL\" = false\ntest \"$NUMBER\" = 0",
    None,
  )])?;
  set(
    &mut raw,
    "environmentVariables",
    serde_json::json!([
      {"type":2}, mapping(&[
        ("EMPTY",serde_json::json!({"type":0})),
        ("BOOL",serde_json::json!({"type":5})),
        ("NUMBER",serde_json::json!({"type":6})),
      ])
    ]),
  )?;
  let observed = replay(raw, config(dir.path()))?;
  assert_eq!(
    observed.conclusion,
    Some(Conclusion::Success),
    "{:?}",
    observed.logs
  );
  assert_eq!(observed.starts, 1);
  Ok(())
}
