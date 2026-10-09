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
  let Some((file, size)) = open_regular(path)? else {
    return Ok(None);
  };
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
  Ok(render(&bytes))
}

/// Open the summary file, refusing anything but the regular file the runner
/// created. A job or action container can replace it (its directory is mounted
/// read-write), and following a symlink would make the host upload whatever the
/// link points at.
fn open_regular(path: &Path) -> Result<Option<(std::fs::File, u64)>, String> {
  let link = match std::fs::symlink_metadata(path) {
    Ok(link) => link,
    Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(read_error(&error)),
  };
  let file = match std::fs::File::open(path) {
    Ok(file) => file,
    Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(read_error(&error)),
  };
  let metadata = file.metadata().map_err(|error| read_error(&error))?;
  if !link.file_type().is_file() || !metadata.is_file() || !same_file(&link, &metadata) {
    return Err(not_regular_error());
  }
  Ok(Some((file, metadata.len())))
}

/// Strip a BOM, normalize line endings to the platform newline, and drop a file
/// that holds no text (empty, or only a BOM).
fn render(bytes: &[u8]) -> Option<String> {
  let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
  let decoded = String::from_utf8_lossy(bytes);
  let normalized = decoded.replace("\r\n", "\n").replace('\r', "\n");
  let newline = if cfg!(windows) { "\r\n" } else { "\n" };
  let mut content = String::new();
  for line in normalized.split_terminator('\n') {
    content.push_str(line);
    content.push_str(newline);
  }
  (!content.is_empty()).then_some(content)
}

/// The path and the opened handle must name one inode, so a file swapped for a
/// symlink between the two lookups is rejected rather than followed.
#[cfg(unix)]
fn same_file(link: &std::fs::Metadata, opened: &std::fs::Metadata) -> bool {
  use std::os::unix::fs::MetadataExt;
  link.dev() == opened.dev() && link.ino() == opened.ino()
}

#[cfg(not(unix))]
fn same_file(_link: &std::fs::Metadata, _opened: &std::fs::Metadata) -> bool {
  true
}

fn not_regular_error() -> String {
  "$GITHUB_STEP_SUMMARY upload aborted: unable to read summary file (not a regular file)".to_owned()
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
