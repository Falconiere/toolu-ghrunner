//! `watch::state` reducer over the committed REAL-engine fixture: job/step
//! reconstruction (AC-3), running-vs-done badges + bounded log ring (AC-5),
//! and the seq-gap warning flag (AC-8, state half).

use std::error::Error;
use std::path::PathBuf;

use toolu_runner::journal::{JournalEvent, JournalLine};
use toolu_runner::watch::state::{LOG_RING, OpenJob, StepStatus};

const FIXTURE: &str = include_str!("fixtures/journal/canonical.jsonl");

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn fixture_lines() -> TestResult<Vec<JournalLine>> {
  Ok(
    FIXTURE
      .lines()
      .map(serde_json::from_str)
      .collect::<Result<_, _>>()?,
  )
}

fn reduced(lines: Vec<JournalLine>) -> OpenJob {
  let mut job = OpenJob::new(PathBuf::from("fixture.jsonl"));
  job.apply_all(lines);
  job
}

#[test]
fn full_fixture_reconstructs_the_job() -> TestResult {
  let job = reduced(fixture_lines()?);

  assert_eq!(job.job_name.as_deref(), Some("build"));
  assert_eq!(
    job.job_id.as_deref(),
    Some("0e9d8c7b-3333-4444-8555-666677778888")
  );
  assert_eq!(job.conclusion.as_deref(), Some("success"), "✓ badge");
  assert!(!job.seq_gap, "contiguous fixture must not flag a gap");

  assert_eq!(job.steps.len(), 2, "greet + farewell");
  let numbers: Vec<u32> = job.steps.iter().map(|s| s.number).collect();
  let mut sorted = numbers.clone();
  sorted.sort_unstable();
  assert_eq!(numbers, sorted, "steps ordered by step_number");
  assert!(job.steps.iter().all(|s| s.status == StepStatus::Success));

  let greet = job
    .steps
    .iter()
    .find(|s| s.step_id == "greet")
    .ok_or("greet step missing")?;
  assert_eq!(greet.annotations.len(), 1, "::warning:: attaches to greet");
  let (level, _) = greet.annotations.first().ok_or("annotation missing")?;
  assert_eq!(level, "warning");

  assert!(
    job
      .logs
      .iter()
      .any(|l| l.text.contains("hello from step one")),
    "step stdout reaches the log ring"
  );
  Ok(())
}

#[test]
fn truncated_fixture_reads_as_running() -> TestResult {
  let mut lines = fixture_lines()?;
  let last = lines.pop().ok_or("fixture empty")?;
  assert!(matches!(last.event, JournalEvent::JobCompleted { .. }));

  let job = reduced(lines);
  assert_eq!(job.conclusion, None, "no job_completed → running");
  assert_eq!(job.steps.len(), 2);
  Ok(())
}

#[test]
fn log_ring_is_bounded() -> TestResult {
  // AC-5 (ring half): repeat the fixture's REAL log events past LOG_RING;
  // the ring must hold exactly LOG_RING newest lines.
  let lines = fixture_lines()?;
  let log_lines: Vec<JournalLine> = lines
    .iter()
    .filter(|l| matches!(l.event, JournalEvent::Log { .. }))
    .cloned()
    .collect();
  assert!(!log_lines.is_empty());

  let mut job = reduced(lines.clone());
  let mut seq = lines.len() as u64;
  while job.logs.len() < LOG_RING {
    for l in &log_lines {
      let mut repeat = l.clone();
      repeat.seq = seq;
      seq += 1;
      job.apply(repeat);
    }
  }
  assert_eq!(job.logs.len(), LOG_RING, "ring fills to exactly the cap");
  // One more full round past capacity: length must stay pinned.
  for l in &log_lines {
    let mut repeat = l.clone();
    repeat.seq = seq;
    seq += 1;
    job.apply(repeat);
  }
  assert_eq!(job.logs.len(), LOG_RING, "ring must stay bounded");
  Ok(())
}

#[test]
fn seq_gap_sets_warning_flag_and_still_renders() -> TestResult {
  // AC-8 (state half): drop fixture line seq=5; the flag trips, the rest
  // of the model still builds.
  let lines: Vec<JournalLine> = fixture_lines()?
    .into_iter()
    .filter(|l| l.seq != 5)
    .collect();
  let job = reduced(lines);
  assert!(job.seq_gap, "gap must set the warning flag");
  assert_eq!(job.steps.len(), 2, "model still renders around the gap");
  assert_eq!(job.conclusion.as_deref(), Some("success"));
  Ok(())
}
