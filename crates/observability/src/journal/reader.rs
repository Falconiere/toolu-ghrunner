//! Incremental journal reader (replay + poll-tail) and the jobs-dir
//! scanner behind the `watch` job list.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::types::{JOURNAL_VERSION, JournalEvent, JournalLine};

/// Bytes read from each end of a journal by `scan_jobs` summaries.
const SCAN_WINDOW: usize = 8192;

/// Incremental reader: replays a journal from byte 0, then tails it.
/// Holds one buffered reader open across polls (the tail loop runs every
/// ~250 ms on a single append-only journal), opening lazily on first poll.
#[derive(Debug)]
pub struct JournalReader {
  path: PathBuf,
  offset: u64,
  reader: Option<BufReader<File>>,
}

impl JournalReader {
  /// Reader positioned at the start of `path`.
  pub fn new(path: PathBuf) -> Self {
    Self {
      path,
      offset: 0,
      reader: None,
    }
  }

  /// The journal file this reader tails.
  pub fn path(&self) -> &Path {
    &self.path
  }

  /// Parse the complete lines appended since the last call.
  ///
  /// The offset advances only past the last complete `\n`; a partial
  /// trailing line is re-read on the next poll. Unparseable lines and
  /// lines with an unknown `v` are skipped (forward compatibility), per
  /// the journal contract.
  ///
  /// # Errors
  ///
  /// Propagates I/O errors from opening or reading the journal file.
  pub fn poll(&mut self) -> std::io::Result<Vec<JournalLine>> {
    // Lazily open once and keep the buffered reader; `insert` returns
    // `&mut BufReader` without an unwrap when the slot was empty, and the
    // 8 KiB buffer is reused across polls.
    let offset = self.offset;
    let reader = match self.reader.as_mut() {
      Some(r) => r,
      None => self.reader.insert(BufReader::new(File::open(&self.path)?)),
    };
    reader.seek(SeekFrom::Start(offset))?;
    // Reads only the bytes appended since `offset`, not the whole file.
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf)?;
    let Some(last_nl) = buf.iter().rposition(|&b| b == b'\n') else {
      return Ok(Vec::new());
    };
    // Drop any partial trailing line; keep bytes through the last newline.
    buf.truncate(last_nl + 1);
    // usize→u64 is infallible on supported targets; on a hypothetical
    // failure leave the offset unchanged (re-read next poll) rather than
    // resetting it.
    self.offset = self
      .offset
      .saturating_add(u64::try_from(buf.len()).unwrap_or(0));
    let text = String::from_utf8_lossy(&buf);
    Ok(text.lines().filter_map(parse_line).collect())
  }
}

/// Parse one journal line; `None` (with a debug trace) for foreign or
/// corrupt lines — reader tolerance is part of the v1 contract.
fn parse_line(raw: &str) -> Option<JournalLine> {
  match serde_json::from_str::<JournalLine>(raw) {
    Ok(line) if line.v == JOURNAL_VERSION => Some(line),
    Ok(line) => {
      tracing::debug!(v = line.v, "journal: skipping line with unknown version");
      None
    },
    Err(e) => {
      tracing::debug!(error = %e, "journal: skipping unparseable line");
      None
    },
  }
}

/// One journal file summarized for the job list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSummary {
  /// Journal file path (feed to `JournalReader::new` to open the job).
  pub path: PathBuf,
  /// Job id from `job_acquired`.
  pub job_id: String,
  /// Job name from `job_started`, if the head window reached it.
  pub job_name: Option<String>,
  /// Timestamp of the first journal line (RFC3339).
  pub started: String,
  /// Final conclusion from `job_completed`; `None` = running or aborted.
  pub conclusion: Option<String>,
}

/// Summarize every journal in `jobs_dir`, newest first (name order).
/// Reads only a head + tail window per file, not whole journals.
///
/// # Errors
///
/// Propagates the directory listing error; unreadable or headless
/// individual files are skipped.
pub fn scan_jobs(jobs_dir: &Path) -> std::io::Result<Vec<JobSummary>> {
  let mut paths: Vec<PathBuf> = std::fs::read_dir(jobs_dir)?
    .filter_map(|entry| match entry {
      Ok(e) => Some(e.path()),
      // A failed dirent read drops one journal from the list; trace it so a
      // job silently missing from the TUI is diagnosable.
      Err(e) => {
        tracing::debug!(error = %e, "journal: skipping unreadable directory entry");
        None
      },
    })
    .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
    .collect();
  paths.sort();
  paths.reverse();
  Ok(paths.into_iter().filter_map(|p| summarize(&p)).collect())
}

/// Head+tail-window summary of one journal; `None` if no valid head line.
fn summarize(path: &Path) -> Option<JobSummary> {
  let head_lines = complete_lines(&read_window(path, false)?, false);
  let started = head_lines.first()?.ts.clone();
  let mut job_id = None;
  let mut job_name = None;
  for line in &head_lines {
    if let JournalEvent::JobAcquired { job_id: id, .. } = &line.event {
      job_id = Some(id.clone());
    }
    if let JournalEvent::JobStarted { job_name: name, .. } = &line.event {
      job_name = Some(name.clone());
    }
  }
  Some(JobSummary {
    path: path.to_path_buf(),
    job_id: job_id?,
    job_name,
    started,
    conclusion: tail_conclusion(path),
  })
}

/// Last `job_completed` conclusion in the tail window, if any.
fn tail_conclusion(path: &Path) -> Option<String> {
  complete_lines(&read_window(path, true)?, true)
    .iter()
    .rev()
    .find_map(|l| {
      if let JournalEvent::JobCompleted { conclusion, .. } = &l.event {
        Some(conclusion.clone())
      } else {
        None
      }
    })
}

/// Read the first (or last) `SCAN_WINDOW` bytes of `path`.
fn read_window(path: &Path, tail: bool) -> Option<Vec<u8>> {
  let mut file = std::fs::File::open(path).ok()?;
  if tail {
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(SCAN_WINDOW as u64);
    file.seek(SeekFrom::Start(start)).ok()?;
  }
  let mut buf = vec![0_u8; SCAN_WINDOW];
  let mut read = 0;
  while read < buf.len() {
    let Some(rest) = buf.get_mut(read..) else {
      break;
    };
    match file.read(rest) {
      Ok(0) => break,
      Ok(n) => read += n,
      Err(_) => return None,
    }
  }
  buf.truncate(read);
  Some(buf)
}

/// Parse the complete journal lines inside a window.
///
/// A tail window can start mid-line, so its first `split` fragment is
/// dropped. The last `split` fragment is then always dropped too — for a
/// newline-terminated window it is the trailing empty string, and for a
/// window that ends mid-line it is the incomplete final fragment; either
/// way it is not a complete line. This holds for both head and tail
/// windows (the tail's leading fragment was already removed above).
fn complete_lines(window: &[u8], tail: bool) -> Vec<JournalLine> {
  let text = String::from_utf8_lossy(window);
  let mut parts: Vec<&str> = text.split('\n').collect();
  if tail && parts.len() > 1 {
    parts.remove(0);
  }
  parts.pop();
  parts.into_iter().filter_map(parse_line).collect()
}
