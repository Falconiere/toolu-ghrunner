//! Pure TUI application model: a reducer from journal lines to the job
//! list, step tree, and bounded log ring that `ui` renders. No I/O here —
//! `watch::run` feeds it from `JournalReader` / `scan_jobs`.

use std::collections::VecDeque;
use std::path::PathBuf;

use crate::journal::{JobSummary, JournalEvent, JournalLine};

/// Max log lines retained per opened job (bounded memory).
pub const LOG_RING: usize = 10_000;

/// Which pane owns keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pane {
  /// Left job-list pane.
  #[default]
  Jobs,
  /// Right detail pane (steps + logs).
  Detail,
}

/// Execution state of one step in the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
  /// The step is currently executing.
  Running,
  /// The step finished successfully.
  Success,
  /// The step finished with an error.
  Failure,
  /// The step was cancelled.
  Cancelled,
  /// The step was skipped.
  Skipped,
}

impl StepStatus {
  /// Map a journal conclusion string; unknown strings read as `Failure`.
  fn from_conclusion(c: &str) -> Self {
    match c {
      "success" => Self::Success,
      "cancelled" => Self::Cancelled,
      "skipped" => Self::Skipped,
      _ => Self::Failure,
    }
  }
}

/// One step row in the detail pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepView {
  /// The step's id.
  pub step_id: String,
  /// The step's display name.
  pub name: String,
  /// The step's 1-based position in the job.
  pub number: u32,
  /// The step's current execution state.
  pub status: StepStatus,
  /// `(level, message)` annotations attached to this step.
  pub annotations: Vec<(String, String)>,
}

/// One rendered log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
  /// The id of the step this log line belongs to.
  pub step_id: String,
  /// The log line's rendered text.
  pub text: String,
}

/// Reduced view of one opened journal.
#[derive(Debug, Default)]
pub struct OpenJob {
  /// Path to the journal file this state was folded from.
  pub path: PathBuf,
  /// The job's id, once seen in a `JobAcquired` line.
  pub job_id: Option<String>,
  /// The job's display name, once seen in a `JobStarted` line.
  pub job_name: Option<String>,
  /// The broker session id, once seen in a `SessionCreated` line.
  pub session_id: Option<String>,
  /// The step tree, ordered by step number.
  pub steps: Vec<StepView>,
  /// The bounded log ring (newest lines evict oldest past `LOG_RING`).
  pub logs: VecDeque<LogLine>,
  /// The job's final conclusion, once seen in a `JobCompleted` line.
  pub conclusion: Option<String>,
  /// Set when journal `seq` numbers are non-contiguous (UI warning).
  pub seq_gap: bool,
  last_seq: Option<u64>,
}

impl OpenJob {
  /// Empty model for the journal at `path`.
  pub fn new(path: PathBuf) -> Self {
    Self {
      path,
      ..Self::default()
    }
  }

  /// Fold one journal line into the model.
  pub fn apply(&mut self, line: JournalLine) {
    self.track_seq(line.seq);
    match line.event {
      JournalEvent::SessionCreated { session_id } => self.session_id = Some(session_id),
      JournalEvent::JobAcquired { job_id, .. } => self.job_id = Some(job_id),
      JournalEvent::JobStarted { job_name, .. } => self.job_name = Some(job_name),
      JournalEvent::StepStarted {
        step_id,
        step_name,
        step_number,
      } => self.upsert_step(step_id, step_name, step_number, StepStatus::Running),
      JournalEvent::Log { step_id, line, .. } => self.push_log(step_id, line),
      JournalEvent::LogGroup {
        step_id,
        title,
        open,
      } => {
        if open {
          self.push_log(step_id, format!("▸ {title}"));
        }
      },
      JournalEvent::Annotation {
        step_id,
        level,
        message,
        ..
      } => self.attach_annotation(&step_id, level, message),
      JournalEvent::StepCompleted {
        step_id,
        conclusion,
        ..
      } => self.set_step_status(&step_id, StepStatus::from_conclusion(&conclusion)),
      JournalEvent::StepSkipped { step_id, reason } => {
        self.upsert_step(step_id.clone(), step_id.clone(), 0, StepStatus::Skipped);
        self.attach_annotation(&step_id, "notice".to_owned(), reason);
      },
      JournalEvent::JobCompleted { conclusion, .. } => self.conclusion = Some(conclusion),
      JournalEvent::LockRenewed { .. } | JournalEvent::ReportedStatus { .. } => {},
    }
  }

  /// Fold a batch of lines (one reader poll).
  pub fn apply_all(&mut self, lines: Vec<JournalLine>) {
    for line in lines {
      self.apply(line);
    }
  }

  /// Flag non-contiguous sequence numbers instead of failing. The first
  /// line is always accepted, whatever its `seq` — only a gap *between*
  /// consecutive lines trips the warning.
  fn track_seq(&mut self, seq: u64) {
    if let Some(last) = self.last_seq
      && seq != last + 1
    {
      self.seq_gap = true;
    }
    self.last_seq = Some(seq);
  }

  /// Insert a step (ordered by number) or refresh an existing row. A real
  /// `StepStarted` (`number > 0`) upgrades a `StepSkipped` placeholder that
  /// stored `step_id` as its name with `number == 0`.
  fn upsert_step(&mut self, step_id: String, name: String, number: u32, status: StepStatus) {
    if let Some(step) = self.steps.iter_mut().find(|s| s.step_id == step_id) {
      step.status = status;
      if number > 0 {
        step.name = name;
        step.number = number;
        self.steps.sort_by_key(|s| s.number);
      }
      return;
    }
    self.steps.push(StepView {
      step_id,
      name,
      number,
      status,
      annotations: Vec::new(),
    });
    self.steps.sort_by_key(|s| s.number);
  }

  fn set_step_status(&mut self, step_id: &str, status: StepStatus) {
    if let Some(step) = self.steps.iter_mut().find(|s| s.step_id == step_id) {
      step.status = status;
    }
  }

  fn attach_annotation(&mut self, step_id: &str, level: String, message: String) {
    if let Some(step) = self.steps.iter_mut().find(|s| s.step_id == step_id) {
      step.annotations.push((level, message));
    }
  }

  /// Append to the bounded log ring, evicting the oldest line at capacity.
  fn push_log(&mut self, step_id: String, text: String) {
    if self.logs.len() >= LOG_RING {
      self.logs.pop_front();
    }
    self.logs.push_back(LogLine { step_id, text });
  }
}

/// Top-level TUI state: job list + optional opened job.
#[derive(Debug, Default)]
pub struct App {
  /// All discovered jobs, most recent first.
  pub jobs: Vec<JobSummary>,
  /// Index of the currently highlighted row in `jobs`.
  pub selected: usize,
  /// The currently opened job's folded state, if any.
  pub opened: Option<OpenJob>,
  /// Which pane currently owns keyboard focus.
  pub pane: Pane,
  /// Whether the log pane auto-scrolls to the newest line.
  pub follow: bool,
  /// Cancel confirmation pending (`c` pressed, awaiting `y`/`n`).
  pub confirm_cancel: bool,
  /// Log-pane scroll offset from the bottom (0 = pinned to tail).
  pub scroll_from_bottom: usize,
  /// Header line for runner identity (`<unregistered>` fallback).
  pub runner_name: String,
  /// Header line for `.lock` holder state (`idle` / `running pid=…`).
  pub lock_line: String,
  /// One-shot status message shown in the footer (e.g. cancel outcome).
  pub flash: Option<String>,
}

impl App {
  /// Fresh state with a runner display name.
  pub fn new(runner_name: String) -> Self {
    Self {
      runner_name,
      follow: true,
      ..Self::default()
    }
  }

  /// Replace the job list, clamping the selection.
  pub fn set_jobs(&mut self, jobs: Vec<JobSummary>) {
    self.jobs = jobs;
    if self.selected >= self.jobs.len() {
      self.selected = self.jobs.len().saturating_sub(1);
    }
  }

  /// Move the job-list cursor one row up.
  pub fn select_up(&mut self) {
    self.selected = self.selected.saturating_sub(1);
  }

  /// Move the job-list cursor one row down.
  pub fn select_down(&mut self) {
    if !self.jobs.is_empty() {
      self.selected = (self.selected + 1).min(self.jobs.len() - 1);
    }
  }
}
