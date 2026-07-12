//! Azure "Put Block List": the commit. The XML body lists block ids in commit
//! order (`<Latest>` / `<Committed>` / `<Uncommitted>` are all treated as
//! "include this id, in document order"). The referenced blocks are
//! concatenated into the staging file and the per-block temp data is dropped.

use std::fs;
use std::path::Path;

use axum::body::Bytes;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use shared::RunnerError;

use super::put_block::{block_filename, blocks_dir};

/// Assemble the committed blocks into `staging`, then drop the blocks directory.
///
/// # Errors
/// `RunnerError::Cache` if the XML is malformed, `RunnerError::Io` if a block
/// is missing or the staging file cannot be written.
pub(super) async fn commit(staging: &Path, body: Bytes) -> Result<(), RunnerError> {
  let ids = parse_block_list(&body)?;
  let staging = staging.to_path_buf();
  tokio::task::spawn_blocking(move || assemble(&staging, &ids))
    .await
    .map_err(|e| RunnerError::Cache(format!("block-list assemble join failed: {e}")))?
}

/// Parse the block-list XML into the ordered list of block ids to commit.
fn parse_block_list(body: &[u8]) -> Result<Vec<String>, RunnerError> {
  let text = std::str::from_utf8(body)
    .map_err(|e| RunnerError::Cache(format!("block list not utf8: {e}")))?;
  let mut reader = Reader::from_str(text);
  let mut ids = Vec::new();
  let mut capture = false;
  loop {
    let event = reader
      .read_event()
      .map_err(|e| RunnerError::Cache(format!("block list xml: {e}")))?;
    if let Event::Eof = &event {
      break;
    }
    if let Event::Start(start) = &event {
      if is_id_element(start) {
        capture = true;
      }
    } else if let Event::End(_) = &event {
      capture = false;
    } else if let Event::Text(bytes) = &event {
      collect_id(bytes, capture, &mut ids)?;
    }
  }
  Ok(ids)
}

/// Push the trimmed, non-empty text of a captured id element onto `ids`.
fn collect_id(
  bytes: &quick_xml::events::BytesText<'_>,
  capture: bool,
  ids: &mut Vec<String>,
) -> Result<(), RunnerError> {
  if !capture {
    return Ok(());
  }
  let text = bytes
    .unescape()
    .map_err(|e| RunnerError::Cache(format!("block id decode: {e}")))?;
  let id = text.trim();
  if !id.is_empty() {
    ids.push(id.to_owned());
  }
  Ok(())
}

/// True if `start` is one of the block-id-bearing elements.
fn is_id_element(start: &BytesStart<'_>) -> bool {
  matches!(
    start.local_name().as_ref(),
    b"Latest" | b"Committed" | b"Uncommitted"
  )
}

/// Concatenate the referenced blocks into `staging`, then remove the blocks dir.
fn assemble(staging: &Path, ids: &[String]) -> Result<(), RunnerError> {
  let dir = blocks_dir(staging);
  if let Some(parent) = staging.parent() {
    fs::create_dir_all(parent).map_err(RunnerError::Io)?;
  }
  let mut out = fs::File::create(staging).map_err(RunnerError::Io)?;
  for id in ids {
    let path = dir.join(block_filename(id));
    let mut src = fs::File::open(&path).map_err(RunnerError::Io)?;
    std::io::copy(&mut src, &mut out).map_err(RunnerError::Io)?;
  }
  out.sync_all().map_err(RunnerError::Io)?;
  let _ = fs::remove_dir_all(&dir);
  Ok(())
}
