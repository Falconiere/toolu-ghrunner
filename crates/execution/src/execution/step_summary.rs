//! Bounded summary snapshots, isolated from other file-command processing.

use std::io::{ErrorKind, Read};
use std::path::Path;

use shared::{AnnotationLevel, RunnerEvent};
use tokio::sync::mpsc;

use super::context::ExecutionContext;

const MAX_BYTES: u64 = 1024 * 1024;

/// Collect one finished handler's summary without changing its conclusion.
pub(crate) async fn collect(
  path: &Path,
  report_id: &str,
  summary_id: &str,
  ctx: &ExecutionContext,
  events: &mpsc::Sender<RunnerEvent>,
) {
  let path = path.to_owned();
  let result = tokio::task::spawn_blocking(move || read_summary(&path)).await;
  let event = match result {
    Ok(Ok(Some(content))) => {
      let content = match ctx.masker().lock() {
        Ok(masker) => masker.mask(&content).into_owned(),
        Err(poisoned) => poisoned.into_inner().mask(&content).into_owned(),
      };
      RunnerEvent::StepSummary {
        step_id: summary_id.to_owned(),
        content,
      }
    },
    Ok(Ok(None)) => return,
    Ok(Err(message)) => annotation(report_id, message),
    Err(_) => annotation(
      report_id,
      "$GITHUB_STEP_SUMMARY upload aborted: summary reader failed".to_owned(),
    ),
  };
  if events.send(event).await.is_err() {
    tracing::warn!("event receiver closed while collecting step summary");
  }
}

fn read_summary(path: &Path) -> Result<Option<String>, String> {
  let file = match std::fs::File::open(path) {
    Ok(file) => file,
    Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(read_error(&error)),
  };
  let size = file.metadata().map_err(|error| read_error(&error))?.len();
  if size > MAX_BYTES {
    return Err(oversized(size));
  }
  let mut bytes = Vec::new();
  file
    .take(MAX_BYTES + 1)
    .read_to_end(&mut bytes)
    .map_err(|error| read_error(&error))?;
  let size = u64::try_from(bytes.len())
    .map_err(|error| format!("summary size conversion failed: {error}"))?;
  if size > MAX_BYTES {
    return Err(oversized(size));
  }
  if bytes.is_empty() {
    return Ok(None);
  }
  let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
  let decoded = String::from_utf8_lossy(bytes);
  let normalized = decoded.replace("\r\n", "\n").replace('\r', "\n");
  let newline = if cfg!(windows) { "\r\n" } else { "\n" };
  let mut content = String::new();
  for line in normalized.split_terminator('\n') {
    content.push_str(line);
    content.push_str(newline);
  }
  Ok(Some(content))
}

fn read_error(error: &std::io::Error) -> String {
  format!(
    "$GITHUB_STEP_SUMMARY upload aborted: unable to read summary file ({:?})",
    error.kind()
  )
}

fn oversized(size: u64) -> String {
  format!(
    "$GITHUB_STEP_SUMMARY upload aborted, supports content up to a size of 1024k, got {}k. For more information see: https://docs.github.com/actions/using-workflows/workflow-commands-for-github-actions#adding-a-markdown-summary",
    size / 1024
  )
}

fn annotation(step_id: &str, message: String) -> RunnerEvent {
  RunnerEvent::Annotation {
    step_id: step_id.to_owned(),
    level: AnnotationLevel::Error,
    message,
    file: None,
    line: None,
    end_line: None,
    col: None,
    end_column: None,
    title: None,
  }
}
