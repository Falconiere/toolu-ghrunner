//! Incremental journal reader (replay + poll-tail) and the jobs-dir
//! scanner behind the `watch` job list.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::types::{JOURNAL_VERSION, JournalEvent, JournalLine};

/// Bytes read from each end of a journal by `scan_jobs` summaries.
const SCAN_WINDOW: usize = 8192;

/// Incremental reader: replays a journal from byte 0, then tails it.
#[derive(Debug)]
pub struct JournalReader {
  path: PathBuf,
  offset: u64,
}

impl JournalReader {
  /// Reader positioned at the start of `path`.
  pub fn new(path: PathBuf) -> Self {
    Self { path, offset: 0 }
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
    let mut file = std::io::BufReader::new(std::fs::File::open(&self.path)?);
    file.seek(SeekFrom::Start(self.offset))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    let Some(last_nl) = buf.iter().rposition(|&b| b == b'\n') else {
      return Ok(Vec::new());
    };
    let Some(complete) = buf.get(..=last_nl) else {
      return Ok(Vec::new());
    };
    self.offset += u64::try_from(last_nl).unwrap_or_default() + 1;
    let text = String::from_utf8_lossy(complete);
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
    .filter_map(Result::ok)
    .map(|e| e.path())
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

/// Parse the complete journal lines inside a window. A tail window may
/// start mid-line, so its first fragment is dropped; a head window may end
/// mid-line, so its trailing fragment is dropped.
fn complete_lines(window: &[u8], tail: bool) -> Vec<JournalLine> {
  let text = String::from_utf8_lossy(window);
  let mut parts: Vec<&str> = text.split('\n').collect();
  if tail && parts.len() > 1 {
    parts.remove(0);
  }
  // `split` yields a trailing "" for newline-terminated text; dropping the
  // last part removes either that or an incomplete head fragment.
  parts.pop();
  parts.into_iter().filter_map(parse_line).collect()
}
