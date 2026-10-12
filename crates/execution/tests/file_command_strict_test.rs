//! Issue #96: strict `GITHUB_ENV` / `GITHUB_OUTPUT` / `GITHUB_STATE` parsing
//! through the production engine. Every replay starts from the sanitized #68
//! GitHub.com acquisition and only swaps scripts, conditions, step inputs,
//! `continueOnError` and one secret variable; real Bash, Node and composite
//! actions write the file commands.

use std::error::Error;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use execution::Runner;
use execution::node::runtime::{node_binary_path, node_cache_dir, node_version_for};
use shared::{
  AgentJobRequestMessage, AnnotationLevel, Conclusion, DictEntry, RunnerConfig, RunnerEvent,
  SecretMasker, TemplateToken, VariableValue,
};
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;
const CAPTURE: &str = include_str!("incoming_contexts_matrix_0.json");
const BLOCKED: &str = "Can't store NODE_OPTIONS output parameter using '$GITHUB_ENV' command.";

fn literal(value: &str) -> TemplateToken {
  TemplateToken {
    token_type: 0,
    lit: Some(value.to_owned()),
    ..TemplateToken::default()
  }
}

fn mapping(values: &[(&str, &str)]) -> TemplateToken {
  TemplateToken {
    token_type: 2,
    d: Some(
      values
        .iter()
        .map(|(key, value)| DictEntry {
          key: literal(key),
          value: literal(value),
        })
        .collect(),
    ),
    ..TemplateToken::default()
  }
}

/// The captured job reduced to the steps whose context names are listed, in
/// capture order.
fn job(names: &[&str]) -> TestResult<AgentJobRequestMessage> {
  let mut job: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  job
    .steps
    .retain(|step| names.contains(&step.context_name.as_deref().unwrap_or_default()));
  Ok(job)
}

fn step<'a>(
  job: &'a mut AgentJobRequestMessage,
  name: &str,
) -> TestResult<&'a mut shared::ActionStep> {
  Ok(
    job
      .steps
      .iter_mut()
      .find(|step| step.context_name.as_deref() == Some(name))
      .ok_or("captured step absent")?,
  )
}

fn set_script(job: &mut AgentJobRequestMessage, name: &str, body: &str) -> TestResult {
  let entry = step(job, name)?
    .inputs
    .d
    .as_mut()
    .and_then(|entries| {
      entries
        .iter_mut()
        .find(|entry| entry.key.to_string_value() == Some("script"))
    })
    .ok_or("captured script token absent")?;
  entry.value = literal(body);
  Ok(())
}

/// Three script steps: the writer `__run`, a default-`success()` reader
/// `__run_2`, and an `always()` cleanup `__run_3`.
fn scripts(writer: &str, reader: &str) -> TestResult<AgentJobRequestMessage> {
  let mut job = job(&["__run", "__run_2", "__run_3"])?;
  set_script(&mut job, "__run", writer)?;
  set_script(&mut job, "__run_2", reader)?;
  set_script(&mut job, "__run_3", "printf 'FC96|cleanup\\n'")?;
  for name in ["__run_2", "__run_3"] {
    let step = step(&mut job, name)?;
    step.environment = None;
    // The captured `__run_2` runs in a directory the replaced `__run` made.
    if let Some(entries) = step.inputs.d.as_mut() {
      entries.retain(|entry| entry.key.to_string_value() != Some("workingDirectory"));
    }
  }
  step(&mut job, "__run_3")?.condition = Some("always()".to_owned());
  Ok(job)
}

/// Give the writer a user-chosen `id:` so `steps.writer.*` resolves; generated
/// `__run` names are not addressable from expressions.
fn name_writer(job: &mut AgentJobRequestMessage) -> TestResult {
  step(job, "__run")?.context_name = Some("writer".to_owned());
  Ok(())
}

/// The local `__self` action step pointed at a fixture with `with:` inputs.
fn local_action(action: &str, inputs: &[(&str, &str)]) -> TestResult<AgentJobRequestMessage> {
  let mut job = job(&["__self"])?;
  let step = step(&mut job, "__self")?;
  step.reference.path = Some(format!("./.github/actions/{action}"));
  step.inputs = mapping(inputs);
  Ok(job)
}

async fn seed_node(data: &Path) -> TestResult {
  let output = tokio::process::Command::new("node")
    .args(["-e", "process.stdout.write(process.execPath)"])
    .output()
    .await?;
  assert!(output.status.success(), "Node is required for this test");
  let host = String::from_utf8(output.stdout)?;
  for major in [20, 24] {
    let target = node_binary_path(&node_cache_dir(data, node_version_for(major)));
    tokio::fs::create_dir_all(target.parent().ok_or("node cache parent missing")?).await?;
    tokio::fs::symlink(host.trim(), target).await?;
  }
  Ok(())
}

async fn replay(job: AgentJobRequestMessage, actions: &[&str]) -> TestResult<Replay> {
  let root = tempfile::tempdir()?;
  let config = RunnerConfig {
    data_dir: root.path().join("data"),
    workspace_root: root.path().join("work"),
    workspace_gc_hours: 0,
    workspace: shared::WorkspaceConfig {
      mode: shared::WorkspaceMode::Fresh,
      stable_gc_hours: 0,
    },
    ..RunnerConfig::default()
  };
  seed_node(&config.data_dir).await?;
  let workspace = config.workspace_root.join(&job.job_id);
  for action in actions {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
      .join("../toolu-runner/tests/fixtures/local_actions")
      .join(action);
    let target = workspace.join(".github/actions").join(action);
    std::fs::create_dir_all(&target)?;
    for entry in std::fs::read_dir(source)? {
      let entry = entry?;
      std::fs::copy(entry.path(), target.join(entry.file_name()))?;
    }
  }
  let ids = job
    .steps
    .iter()
    .map(|step| {
      (
        step.context_name.clone().unwrap_or_default(),
        step.id.clone(),
      )
    })
    .collect();
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut events = runner.execute_job(job, CancellationToken::new());
  let mut seen = Vec::new();
  tokio::time::timeout(Duration::from_secs(240), async {
    while let Some(event) = events.recv().await {
      seen.push(event);
    }
  })
  .await?;
  Ok(Replay { events: seen, ids })
}

struct Replay {
  events: Vec<RunnerEvent>,
  ids: Vec<(String, String)>,
}

impl Replay {
  fn id(&self, name: &str) -> String {
    self
      .ids
      .iter()
      .find(|(context, _)| context == name)
      .map(|(_, id)| id.clone())
      .unwrap_or_default()
  }

  fn job_conclusion(&self) -> Option<Conclusion> {
    self.events.iter().find_map(|event| {
      if let RunnerEvent::JobCompleted { conclusion, .. } = event {
        Some(*conclusion)
      } else {
        None
      }
    })
  }

  fn step_conclusion(&self, name: &str) -> Option<Conclusion> {
    let id = self.id(name);
    self.events.iter().find_map(|event| {
      if let RunnerEvent::StepCompleted {
        step_id,
        conclusion,
        ..
      } = event
        && *step_id == id
      {
        Some(*conclusion)
      } else {
        None
      }
    })
  }

  /// Outputs the named step completed with.
  fn outputs(&self, name: &str) -> Option<std::collections::HashMap<String, String>> {
    let id = self.id(name);
    self.events.iter().find_map(|event| {
      if let RunnerEvent::StepCompleted {
        step_id, outputs, ..
      } = event
        && *step_id == id
      {
        Some(outputs.clone())
      } else {
        None
      }
    })
  }

  /// Error annotations reported on the named step's timeline record.
  fn errors(&self, name: &str) -> Vec<String> {
    let id = self.id(name);
    self
      .events
      .iter()
      .filter_map(|event| {
        if let RunnerEvent::Annotation {
          step_id,
          level: AnnotationLevel::Error,
          message,
          ..
        } = event
          && *step_id == id
        {
          Some(message.clone())
        } else {
          None
        }
      })
      .collect()
  }

  fn lines(&self) -> Vec<&str> {
    self
      .events
      .iter()
      .filter_map(|event| {
        if let RunnerEvent::Log { line, .. } = event {
          Some(line.as_str())
        } else {
          None
        }
      })
      .collect()
  }

  /// Log lines starting with `prefix`, in order.
  fn marked(&self, prefix: &str) -> Vec<String> {
    self
      .lines()
      .into_iter()
      .filter(|line| line.starts_with(prefix))
      .map(ToOwned::to_owned)
      .collect()
  }
}

fn hex(value: &str) -> String {
  value
    .bytes()
    .flat_map(|byte| [byte >> 4, byte & 0x0f])
    .filter_map(|nibble| char::from_digit(u32::from(nibble), 16))
    .collect()
}

#[tokio::test]
async fn valid_bodies_reach_env_and_outputs_exactly() -> TestResult {
  let writer = concat!(
    "printf 'EMPTY=\\nEQ=b=c\\nMIXED=x<<y\\nU=h\u{e9}llo \u{2713}\\nCR=1\\r\\nDUP=1\\nDUP=2\\n' >> \"$GITHUB_ENV\"\n",
    "printf 'H<<EOF\\na\\n\\nb\\nEOF\\nHE<<EOF\\nEOF\\nLIKE<<EOF\\nEOF2\\n EOF\\nEOF\\nHCR<<EOF\\r\\nv\\r\\nEOF\\r\\n' >> \"$GITHUB_ENV\"\n",
    "printf 'out=b=c\\nmulti<<EOF\\nl1\\nl2\\nEOF\\n' >> \"$GITHUB_OUTPUT\"\n",
  );
  let reader = concat!(
    "for n in EMPTY EQ MIXED U CR DUP H HE LIKE HCR MULTI OUT; do\n",
    "  printf 'FC96|%s|%s\\n' \"$n\" \"$(printf '%s' \"${!n}\" | od -An -tx1 | tr -d ' \\n')\"\n",
    "done\n",
  );
  let mut job = scripts(writer, reader)?;
  step(&mut job, "__run_2")?.environment = Some(TemplateToken {
    token_type: 2,
    d: Some(vec![
      DictEntry {
        key: literal("MULTI"),
        value: TemplateToken {
          token_type: 3,
          expr: Some("steps.writer.outputs.multi".to_owned()),
          ..TemplateToken::default()
        },
      },
      DictEntry {
        key: literal("OUT"),
        value: TemplateToken {
          token_type: 3,
          expr: Some("steps.writer.outputs.out".to_owned()),
          ..TemplateToken::default()
        },
      },
    ]),
    ..TemplateToken::default()
  });
  name_writer(&mut job)?;
  let run = replay(job, &[]).await?;
  assert_eq!(
    run.job_conclusion(),
    Some(Conclusion::Success),
    "{:?}",
    run.events
  );
  let expected: Vec<String> = [
    ("EMPTY", ""),
    ("EQ", "b=c"),
    ("MIXED", "x<<y"),
    ("U", "h\u{e9}llo \u{2713}"),
    ("CR", "1\r"),
    ("DUP", "2"),
    ("H", "a\n\nb"),
    ("HE", ""),
    ("LIKE", "EOF2\n EOF"),
    ("HCR", "v\r"),
    ("MULTI", "l1\nl2"),
    ("OUT", "b=c"),
  ]
  .iter()
  .map(|(name, value)| format!("FC96|{name}|{}", hex(value)))
  .collect();
  assert_eq!(
    run.marked("FC96|"),
    [expected, vec!["FC96|cleanup".to_owned()]].concat()
  );
  assert!(run.errors("writer").is_empty());
  Ok(())
}

#[tokio::test]
async fn malformed_env_fails_the_step_with_upstream_issues_and_runs_cleanup() -> TestResult {
  let cases = [
    (
      "N<<EOF\\nv\\n",
      "Invalid value. Matching delimiter not found 'EOF'",
    ),
    ("N<<EOF\\nv", "Invalid value. EOF marker missing new line."),
    ("just-text\\n", "Invalid format 'just-text'"),
    ("\\r\\n", "Invalid format '\r'"),
    ("=v\\n", "Invalid format '=v'. Name must not be empty"),
    (
      "N<<\\n",
      "Invalid format 'N<<'. Name must not be empty and delimiter must not be empty",
    ),
  ];
  for (body, message) in cases {
    let writer = format!("printf '{body}' >> \"$GITHUB_ENV\"");
    let run = replay(scripts(&writer, "printf 'FC96|reader\\n'")?, &[]).await?;
    assert_eq!(
      run.step_conclusion("__run"),
      Some(Conclusion::Failure),
      "{body}"
    );
    let issues = vec![
      "Unable to process file command 'env' successfully.".to_owned(),
      message.to_owned(),
    ];
    assert_eq!(run.errors("__run"), issues, "{body}");
    for issue in &issues {
      let line = format!("##[error]{issue}");
      assert!(run.lines().contains(&line.as_str()), "{body}: {line}");
    }
    assert_eq!(run.marked("FC96|"), ["FC96|cleanup"], "{body}");
    assert_eq!(run.job_conclusion(), Some(Conclusion::Failure), "{body}");
  }
  Ok(())
}

#[tokio::test]
async fn malformed_output_and_state_fail_a_script_step() -> TestResult {
  for (file, command, body, message) in [
    (
      "GITHUB_OUTPUT",
      "output",
      "o<<EOF\\n",
      "Invalid value. Matching delimiter not found 'EOF'",
    ),
    ("GITHUB_STATE", "state", "bad\\n", "Invalid format 'bad'"),
  ] {
    let writer = format!("printf '{body}' >> \"${file}\"");
    let run = replay(scripts(&writer, "true")?, &[]).await?;
    assert_eq!(
      run.step_conclusion("__run"),
      Some(Conclusion::Failure),
      "{file}"
    );
    assert_eq!(
      run.errors("__run"),
      [
        format!("Unable to process file command '{command}' successfully."),
        message.to_owned()
      ],
      "{file}"
    );
    assert_eq!(run.job_conclusion(), Some(Conclusion::Failure), "{file}");
  }
  Ok(())
}

#[tokio::test]
async fn pairs_before_the_error_apply_and_continue_on_error_keeps_the_job_green() -> TestResult {
  let writer = concat!(
    "printf 'OK=1\\nbad\\nLATER=2\\n' >> \"$GITHUB_ENV\"\n",
    "printf 'kept=yes\\n' >> \"$GITHUB_OUTPUT\"\n",
  );
  let reader = "printf 'FC96|%s|%s|%s|%s|%s\\n' \"${OK-unset}\" \"${LATER-unset}\" '${{ steps.writer.outputs.kept }}' '${{ steps.writer.outcome }}' '${{ steps.writer.conclusion }}'";
  let mut job = scripts(writer, reader)?;
  step(&mut job, "__run")?.continue_on_error = Some(TemplateToken {
    token_type: 5,
    bool_val: Some(true),
    ..TemplateToken::default()
  });
  name_writer(&mut job)?;
  let run = replay(job, &[]).await?;
  assert_eq!(
    run.marked("FC96|"),
    ["FC96|1|unset|yes|failure|success", "FC96|cleanup"]
  );
  assert_eq!(
    run.errors("writer"),
    [
      "Unable to process file command 'env' successfully.",
      "Invalid format 'bad'"
    ]
  );
  assert_eq!(run.job_conclusion(), Some(Conclusion::Success));
  Ok(())
}

#[tokio::test]
async fn node_options_is_blocked_with_an_issue_but_the_step_succeeds() -> TestResult {
  let writer = "printf 'NODE_OPTIONS=--require /tmp/fc96.js\\nnode_options=--require /tmp/fc96.js\\nKEEP=1\\n' >> \"$GITHUB_ENV\"";
  // Hermetic: a host NODE_OPTIONS may reach the step, but never the blocked value.
  let reader = "if [ \"${NODE_OPTIONS-}\" = '--require /tmp/fc96.js' ]; then v=applied; else v=not-applied; fi; printf 'FC96|%s|%s\\n' \"$v\" \"$KEEP\"";
  let run = replay(scripts(writer, reader)?, &[]).await?;
  assert_eq!(run.marked("FC96|"), ["FC96|not-applied|1", "FC96|cleanup"]);
  // Upstream raises one issue per blocked pair, whatever its case.
  assert_eq!(run.errors("__run"), [BLOCKED, BLOCKED]);
  assert_eq!(run.step_conclusion("__run"), Some(Conclusion::Success));
  assert_eq!(run.job_conclusion(), Some(Conclusion::Success));
  Ok(())
}

#[tokio::test]
async fn a_secret_in_a_malformed_line_is_masked_everywhere() -> TestResult {
  const SECRET: &str = "fc96-s3cr3t-value";
  let mut job = scripts(
    &format!("printf 'bad {SECRET}\\n' >> \"$GITHUB_ENV\""),
    "true",
  )?;
  job.variables.insert(
    "FC96_SECRET".to_owned(),
    VariableValue {
      value: SECRET.to_owned(),
      is_secret: true,
    },
  );
  let run = replay(job, &[]).await?;
  assert_eq!(
    run.errors("__run"),
    [
      "Unable to process file command 'env' successfully.",
      "Invalid format 'bad ***'"
    ]
  );
  assert!(run.lines().contains(&"##[error]Invalid format 'bad ***'"));
  // Annotations reach the Run Service and journal as emitted, so every one
  // must be masked here. Plain log lines are masked by each consumer (the
  // engine's event stream is unmasked by design), but the issue lines this
  // change emits are masked at the source.
  let leaked: Vec<_> = run
    .events
    .iter()
    .filter(|event| {
      matches!(event, RunnerEvent::Annotation { .. })
        || matches!(event, RunnerEvent::Log { line, .. } if line.starts_with("##[error]"))
    })
    .filter(|event| format!("{event:?}").contains(SECRET))
    .collect();
  assert!(leaked.is_empty(), "secret leaked: {leaked:?}");
  Ok(())
}

#[tokio::test]
async fn composite_run_step_malformed_output_fails_the_parent_step() -> TestResult {
  let job = local_action(
    "composite-96-output",
    &[("target", "OUTPUT"), ("body", "ok=1\\nbad\\n")],
  )?;
  let run = replay(job, &["composite-96-output"]).await?;
  assert_eq!(run.step_conclusion("__self"), Some(Conclusion::Failure));
  assert_eq!(
    run.errors("__self"),
    [
      "Unable to process file command 'output' successfully.",
      "Invalid format 'bad'"
    ]
  );
  // `ok=1` precedes the malformed line, so it still reaches the action output.
  assert_eq!(
    run
      .outputs("__self")
      .and_then(|outputs| outputs.get("ok").cloned()),
    Some("1".to_owned())
  );
  assert_eq!(run.marked("CMP96|"), ["CMP96|cleanup"]);
  assert_eq!(run.job_conclusion(), Some(Conclusion::Failure));
  Ok(())
}

#[tokio::test]
async fn composite_run_step_node_options_is_blocked_without_failing() -> TestResult {
  let job = local_action(
    "composite-96-output",
    &[
      ("target", "ENV"),
      ("body", "NODE_OPTIONS=--require /tmp/fc96.js\\n"),
    ],
  )?;
  let run = replay(job, &["composite-96-output"]).await?;
  assert_eq!(
    run.marked("CMP96|"),
    ["CMP96|after|not-applied", "CMP96|cleanup"]
  );
  assert_eq!(run.errors("__self"), [BLOCKED]);
  assert_eq!(run.job_conclusion(), Some(Conclusion::Success));
  Ok(())
}

#[tokio::test]
async fn node_action_malformed_state_fails_main_and_post_still_runs() -> TestResult {
  let run = replay(local_action("node-96-state", &[])?, &["node-96-state"]).await?;
  assert_eq!(run.step_conclusion("__self"), Some(Conclusion::Failure));
  assert_eq!(
    run.errors("__self"),
    [
      "Unable to process file command 'state' successfully.",
      "Invalid value. Matching delimiter not found 'EOF'"
    ]
  );
  assert_eq!(run.marked("NODE96|"), ["NODE96|post|<unset>"]);
  assert_eq!(run.job_conclusion(), Some(Conclusion::Failure));
  Ok(())
}
