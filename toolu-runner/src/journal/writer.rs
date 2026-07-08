//! Journal sink task: consumes the listener's `ListenerEvent` channel and
//! appends one masked JSON line per event to
//! `<jobs_dir>/<UTC ts>-<job_id>.jsonl`. Never blocks or fails the job: on
//! any I/O error it WARNs once, stops writing, and keeps draining the
//! channel so the producer stays unblocked.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{SecondsFormat, Utc};
use shared::ListenerEvent;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::types::{JOURNAL_VERSION, JournalEvent, JournalLine, sanitize_job_id};
use crate::execution::secret_masker::SecretMasker;

/// Max journal files kept in the jobs dir; oldest (by name) pruned first.
pub const JOURNAL_RETAIN: usize = 50;
/// Max pre-acquire events buffered before the oldest are dropped.
pub const PREACQ_BUF: usize = 256;

/// The journal directory for a runner `data_dir`: `<data_dir>/_diag/jobs`.
pub fn jobs_dir_for(data_dir: &std::path::Path) -> PathBuf {
  data_dir.join("_diag").join("jobs")
}

/// Spawn the journal sink for one `run` invocation.
pub fn spawn(
  rx: mpsc::Receiver<ListenerEvent>,
  jobs_dir: PathBuf,
  masker: Arc<Mutex<SecretMasker>>,
) -> JoinHandle<()> {
  tokio::spawn(run(rx, jobs_dir, masker))
}

/// Drain the channel until the sender side closes, journaling as we go.
async fn run(
  mut rx: mpsc::Receiver<ListenerEvent>,
  jobs_dir: PathBuf,
  masker: Arc<Mutex<SecretMasker>>,
) {
  let mut w = Writer::new(jobs_dir, masker);
  while let Some(ev) = rx.recv().await {
    w.handle(&ev).await;
    // Burst-drain whatever else is already queued, then flush once — tail
    // latency tracks event arrival without a syscall per log line.
    while let Ok(next) = rx.try_recv() {
      w.handle(&next).await;
    }
    w.flush().await;
  }
  w.flush().await;
}

/// Sink state: pre-acquire buffer, open journal file, sequence counter.
struct Writer {
  jobs_dir: PathBuf,
  masker: Arc<Mutex<SecretMasker>>,
  /// Events seen before `JobAcquired` names the journal file.
  pre_acquire: VecDeque<(String, JournalEvent)>,
  file: Option<BufWriter<File>>,
  seq: u64,
  /// Set after the first write error; journaling stops, draining continues.
  failed: bool,
  warned_buf_overflow: bool,
}

impl Writer {
  fn new(jobs_dir: PathBuf, masker: Arc<Mutex<SecretMasker>>) -> Self {
    Self {
      jobs_dir,
      masker,
      pre_acquire: VecDeque::new(),
      file: None,
      seq: 0,
      failed: false,
      warned_buf_overflow: false,
    }
  }

  /// Route one event: open the journal on `JobAcquired`, write if open,
  /// buffer if not yet acquired.
  async fn handle(&mut self, ev: &ListenerEvent) {
    if self.failed {
      return;
    }
    let event = JournalEvent::from(ev);
    if let ListenerEvent::JobAcquired { job_id, .. } = ev {
      self.open_for_job(job_id).await;
      self.write_event(now_rfc3339(), event).await;
    } else if self.file.is_some() {
      self.write_event(now_rfc3339(), event).await;
    } else {
      self.buffer_pre_acquire(event);
    }
  }

  /// Buffer an event that arrived before the job id is known.
  fn buffer_pre_acquire(&mut self, event: JournalEvent) {
    if self.pre_acquire.len() >= PREACQ_BUF {
      self.pre_acquire.pop_front();
      if !self.warned_buf_overflow {
        self.warned_buf_overflow = true;
        tracing::warn!(
          cap = PREACQ_BUF,
          "journal: pre-acquire buffer full; dropping oldest events"
        );
      }
    }
    self.pre_acquire.push_back((now_rfc3339(), event));
  }

  /// Close any current journal, prune retention, open `<ts>-<job_id>.jsonl`,
  /// and replay the pre-acquire buffer into it.
  async fn open_for_job(&mut self, job_id: &str) {
    if let Some(mut f) = self.file.take() {
      if let Err(e) = f.flush().await {
        tracing::warn!(error = %e, "journal: flush on rotation failed");
      }
      self.seq = 0;
    }
    prune_jobs_dir(&self.jobs_dir).await;
    let name = format!(
      "{}-{}.jsonl",
      Utc::now().format("%Y%m%dT%H%M%SZ"),
      sanitize_job_id(job_id)
    );
    let path = self.jobs_dir.join(name);
    let open = async {
      tokio::fs::create_dir_all(&self.jobs_dir).await?;
      OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
    };
    match open.await {
      Ok(f) => self.file = Some(BufWriter::new(f)),
      Err(e) => {
        self.fail_once(
          &e,
          "journal: cannot create journal file; journaling disabled for this run",
        );
        return;
      },
    }
    let buffered: Vec<(String, JournalEvent)> = self.pre_acquire.drain(..).collect();
    for (ts, event) in buffered {
      self.write_event(ts, event).await;
    }
  }

  /// Serialize, mask, and append one line.
  async fn write_event(&mut self, ts: String, event: JournalEvent) {
    if self.file.is_none() {
      return;
    }
    let line = JournalLine {
      v: JOURNAL_VERSION,
      seq: self.seq,
      ts,
      event,
    };
    let json = match serde_json::to_string(&line) {
      Ok(j) => j,
      Err(e) => {
        tracing::warn!(error = %e, seq = line.seq, "journal: event serialization failed; line skipped");
        return;
      },
    };
    // Mask before touching the file. A poisoned masker means a holder
    // panicked mid-mutation, so its secret set may be incomplete and
    // redaction can no longer be trusted — fail CLOSED (stop journaling)
    // rather than risk writing an unmasked secret. Bind to an `Option` so
    // the guard (and the `PoisonError` on the error path) drops before the
    // `&mut self` calls below.
    let masked = self.masker.lock().ok().map(|guard| guard.mask(&json));
    let Some(masked) = masked else {
      self.fail_closed(
        "journal: secret masker lock poisoned; journaling disabled to avoid leaking a secret",
      );
      return;
    };
    let Some(file) = self.file.as_mut() else {
      return;
    };
    if let Err(e) = file.write_all(masked.as_bytes()).await {
      self.fail_once(
        &e,
        "journal: write failed; journaling disabled for this run",
      );
      return;
    }
    if let Err(e) = file.write_all(b"\n").await {
      self.fail_once(
        &e,
        "journal: write failed; journaling disabled for this run",
      );
      return;
    }
    self.seq += 1;
  }

  /// Flush the buffered writer; a failure disables journaling.
  async fn flush(&mut self) {
    if let Some(file) = self.file.as_mut()
      && let Err(e) = file.flush().await
    {
      self.fail_once(
        &e,
        "journal: flush failed; journaling disabled for this run",
      );
    }
  }

  /// WARN once, then go quiet: drop the file and stop journaling.
  fn fail_once(&mut self, e: &std::io::Error, msg: &'static str) {
    if !self.failed {
      tracing::warn!(error = %e, "{msg}");
    }
    self.failed = true;
    self.file = None;
  }

  /// Like `fail_once` but for a non-I/O reason (e.g. a poisoned masker).
  fn fail_closed(&mut self, msg: &'static str) {
    if !self.failed {
      tracing::warn!("{msg}");
    }
    self.failed = true;
    self.file = None;
  }
}

/// Delete the oldest `.jsonl` files (lexicographic name order = time order)
/// so that after the caller creates the next file the dir holds at most
/// `JOURNAL_RETAIN`. Called before creation: with fewer than `JOURNAL_RETAIN`
/// files the guard returns early (nothing to prune yet, the new file still
/// leaves the dir under the cap); at or above it, the `+ 1` prunes down to
/// `JOURNAL_RETAIN - 1` to make room for the file about to be created.
async fn prune_jobs_dir(dir: &std::path::Path) {
  let Some(mut names) = list_journals(dir).await else {
    return;
  };
  names.sort();
  if names.len() < JOURNAL_RETAIN {
    return;
  }
  let excess = names.len() + 1 - JOURNAL_RETAIN;
  for path in names.into_iter().take(excess) {
    if let Err(e) = tokio::fs::remove_file(&path).await {
      tracing::warn!(error = %e, path = %path.display(), "journal: retention prune failed");
    }
  }
}

/// The `.jsonl` paths in `dir`; `None` when the dir is absent (nothing to
/// prune) or the listing fails mid-scan.
async fn list_journals(dir: &std::path::Path) -> Option<Vec<PathBuf>> {
  let mut entries = tokio::fs::read_dir(dir).await.ok()?;
  let mut names: Vec<PathBuf> = Vec::new();
  loop {
    match entries.next_entry().await {
      Ok(Some(entry)) => {
        let path = entry.path();
        if path.extension().is_some_and(|x| x == "jsonl") {
          names.push(path);
        }
      },
      Ok(None) => break,
      Err(e) => {
        tracing::warn!(error = %e, "journal: retention scan failed; skipping prune");
        return None;
      },
    }
  }
  Some(names)
}

/// Current UTC time, RFC3339 with millisecond precision (`…Z`).
fn now_rfc3339() -> String {
  Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
