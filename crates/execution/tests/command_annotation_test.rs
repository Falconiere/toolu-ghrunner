//! Real shell annotation commands replayed inside a captured job envelope.

use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use execution::Runner;
use shared::{
  AgentJobRequestMessage, AnnotationLevel, Conclusion, RunnerConfig, RunnerEvent, SecretMasker,
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, PartialEq, Eq)]
struct Seen {
  level: AnnotationLevel,
  message: String,
  title: Option<String>,
  file: Option<String>,
  line: Option<i32>,
  end_line: Option<i32>,
  col: Option<i32>,
  end_column: Option<i32>,
}

#[tokio::test]
async fn real_shell_commands_keep_ranges_titles_and_masks() -> Result<(), Box<dyn Error>> {
  let mut captured: serde_json::Value =
    serde_json::from_str(include_str!("incoming_contexts_matrix_0.json"))?;
  let steps = captured
    .get_mut("steps")
    .and_then(serde_json::Value::as_array_mut)
    .ok_or("captured steps missing")?;
  let mut script = steps.get(1).ok_or("captured script step missing")?.clone();
  let probe =
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/command_annotation_probe.sh");
  let input = script
    .get_mut("inputs")
    .and_then(|v| v.get_mut("map"))
    .and_then(serde_json::Value::as_array_mut)
    .and_then(|entries| entries.first_mut())
    .and_then(|entry| entry.get_mut("Value"))
    .ok_or("captured script token missing")?;
  input["lit"] = serde_json::Value::String(format!("bash '{}'", probe.display()));
  *steps = vec![script];
  let msg: AgentJobRequestMessage = serde_json::from_value(captured)?;
  let dir = tempfile::tempdir()?;
  let cfg = RunnerConfig {
    data_dir: dir.path().join("data"),
    workspace_root: dir.path().join("work"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  };
  let runner = Runner::new(cfg, Arc::new(Mutex::new(SecretMasker::new())));
  let cancel = CancellationToken::new();
  let mut rx = runner.execute_job(msg, cancel.clone());
  let (seen, conclusion) = tokio::time::timeout(Duration::from_secs(30), async {
    let mut seen = Vec::new();
    let mut conclusion = None;
    while let Some(event) = rx.recv().await {
      match event {
        RunnerEvent::Annotation {
          level,
          message,
          title,
          file,
          line,
          end_line,
          col,
          end_column,
          ..
        } => seen.push(Seen {
          level,
          message,
          title,
          file,
          line,
          end_line,
          col,
          end_column,
        }),
        RunnerEvent::JobCompleted { conclusion: c, .. } => conclusion = Some(c),
        RunnerEvent::JobStarted { .. }
        | RunnerEvent::StepStarted { .. }
        | RunnerEvent::StepCompleted { .. }
        | RunnerEvent::StepSkipped { .. }
        | RunnerEvent::Log { .. }
        | RunnerEvent::LogGroup { .. } => {},
      }
    }
    (seen, conclusion)
  })
  .await?;
  cancel.cancel();
  // An error workflow command annotates the step; the shell still exits successfully.
  assert_eq!(conclusion, Some(Conclusion::Success));
  let [
    full,
    warning,
    notice,
    end_only,
    descending_line,
    multiline,
    end_column_only,
    no_line,
    descending_column,
    invalid_line,
  ] = seen.as_slice()
  else {
    return Err(format!("expected 10 nonblank annotations, got {}", seen.len()).into());
  };
  assert_eq!(
    full,
    &Seen {
      level: AnnotationLevel::Error,
      message: "boom\n***".to_owned(),
      title: Some("Compile: detail".to_owned()),
      file: Some("src,sample.rs".to_owned()),
      line: Some(4),
      end_line: Some(4),
      col: Some(2),
      end_column: Some(8),
    }
  );
  assert_eq!(warning.level, AnnotationLevel::Warning);
  assert_eq!(warning.title.as_deref(), Some("Warn:***"));
  assert_eq!(warning.file.as_deref(), Some("src/***.rs"));
  assert_eq!(warning.end_line, Some(9));
  assert_eq!(notice.level, AnnotationLevel::Notice);
  assert_eq!(notice.file, None);
  assert_eq!((end_only.line, end_only.end_line), (Some(7), Some(7)));
  assert_eq!(
    (descending_line.line, descending_line.end_line),
    (None, None)
  );
  assert_eq!((multiline.col, multiline.end_column), (None, None));
  assert_eq!(
    (end_column_only.col, end_column_only.end_column),
    (Some(9), Some(9))
  );
  assert_eq!((no_line.col, no_line.end_column), (None, None));
  assert_eq!(
    (descending_column.col, descending_column.end_column),
    (None, None)
  );
  assert_eq!((invalid_line.line, invalid_line.end_line), (None, None));
  Ok(())
}
