//! Journal reader over the committed REAL-engine fixture
//! (`tests/fixtures/journal/canonical.jsonl`): whole-file replay,
//! incremental-equals-batch tailing (AC-4), corrupt/foreign-line tolerance
//! (AC-8), fixture-line round-trips (AC-11), and `scan_jobs` summaries
//! (AC-3 partial).

use std::error::Error;
use std::io::Write;
use std::path::{Path, PathBuf};

use observability::journal::{JournalEvent, JournalLine, JournalReader, scan_jobs};

const FIXTURE: &str = include_str!("fixtures/journal/canonical.jsonl");

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// Write `content` as a journal file named like the writer would name it.
fn write_journal(dir: &Path, name: &str, content: &str) -> TestResult<PathBuf> {
  let path = dir.join(name);
  std::fs::write(&path, content)?;
  Ok(path)
}

fn batch_replay(path: PathBuf) -> TestResult<Vec<JournalLine>> {
  Ok(JournalReader::new(path).poll()?)
}

#[test]
fn whole_fixture_replays() -> TestResult {
  let dir = tempfile::tempdir()?;
  let path = write_journal(dir.path(), "20260708T210044Z-fix.jsonl", FIXTURE)?;
  let lines = batch_replay(path)?;
  assert_eq!(lines.len(), FIXTURE.lines().count());
  for (i, line) in lines.iter().enumerate() {
    assert_eq!(line.seq, i as u64, "fixture seq must replay in order");
  }
  assert!(matches!(
    lines.last().map(|l| &l.event),
    Some(JournalEvent::JobCompleted { conclusion, .. }) if conclusion == "success"
  ));
  Ok(())
}

#[test]
fn every_fixture_line_round_trips() -> TestResult {
  // AC-11 (fixture half): real captured lines survive parse → serialize →
  // parse without loss.
  for raw in FIXTURE.lines() {
    let line: JournalLine = serde_json::from_str(raw)?;
    let re = serde_json::to_string(&line)?;
    let back: JournalLine = serde_json::from_str(&re)?;
    assert_eq!(back, line, "round-trip changed: {raw}");
  }
  Ok(())
}

#[test]
fn incremental_appends_equal_batch_replay() -> TestResult {
  // AC-4: feed the fixture in two appends — first cut mid-line — and the
  // concatenated polls must equal the one-shot replay.
  let dir = tempfile::tempdir()?;
  let full = batch_replay(write_journal(dir.path(), "batch.jsonl", FIXTURE)?)?;

  // Cut inside line 7 (mid-line split): take everything up to line 7's
  // midpoint by byte offset.
  let line_starts: Vec<usize> = FIXTURE
    .lines()
    .scan(0, |acc, l| {
      let start = *acc;
      *acc += l.len() + 1;
      Some(start)
    })
    .collect();
  let cut = line_starts.get(7).ok_or("fixture shorter than 8 lines")? + 10;

  let path = dir.path().join("incremental.jsonl");
  let (first_half, second_half) = FIXTURE.as_bytes().split_at_checked(cut).ok_or("cut oob")?;
  std::fs::write(&path, first_half)?;
  let mut reader = JournalReader::new(path.clone());
  let mut got = reader.poll()?;
  assert_eq!(
    got.len(),
    7,
    "only the 7 complete lines may parse before the partial line"
  );

  let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;
  file.write_all(second_half)?;
  file.flush()?;
  got.extend(reader.poll()?);
  assert_eq!(got, full, "incremental tail must equal batch replay");
  Ok(())
}

#[test]
fn corrupt_and_foreign_lines_are_skipped() -> TestResult {
  // AC-8: one garbage line and one v:2 line replay to the fixture minus
  // those lines — skipped, not fatal.
  let mut lines: Vec<String> = FIXTURE.lines().map(str::to_owned).collect();
  *lines.get_mut(5).ok_or("line 5 missing")? = "!!! not json at all".to_owned();
  let mut v2: serde_json::Value = serde_json::from_str(lines.get(9).ok_or("line 9 missing")?)?;
  v2.as_object_mut()
    .ok_or("line 9 not an object")?
    .insert("v".to_owned(), serde_json::Value::from(2));
  *lines.get_mut(9).ok_or("line 9 missing")? = serde_json::to_string(&v2)?;

  let dir = tempfile::tempdir()?;
  let path = write_journal(dir.path(), "corrupt.jsonl", &(lines.join("\n") + "\n"))?;
  let got = batch_replay(path)?;

  let expected: Vec<u64> = FIXTURE
    .lines()
    .enumerate()
    .filter(|(i, _)| *i != 5 && *i != 9)
    .map(|(i, _)| i as u64)
    .collect();
  assert_eq!(got.iter().map(|l| l.seq).collect::<Vec<_>>(), expected);
  Ok(())
}

#[test]
fn scan_jobs_summarizes_newest_first() -> TestResult {
  // AC-3 partial: summaries carry id, name, conclusion; a journal
  // truncated before job_completed reads as running (None).
  let dir = tempfile::tempdir()?;
  write_journal(dir.path(), "20260708T210044Z-full.jsonl", FIXTURE)?;
  let truncated: String = FIXTURE
    .lines()
    .take(FIXTURE.lines().count() - 1)
    .map(|l| format!("{l}\n"))
    .collect();
  write_journal(dir.path(), "20260709T000000Z-running.jsonl", &truncated)?;

  let jobs = scan_jobs(dir.path())?;
  assert_eq!(jobs.len(), 2);
  let newest = jobs.first().ok_or("no jobs scanned")?;
  let oldest = jobs.get(1).ok_or("second job missing")?;
  assert_eq!(
    newest.path.file_name().and_then(|n| n.to_str()),
    Some("20260709T000000Z-running.jsonl"),
    "newest journal must come first"
  );
  assert_eq!(newest.conclusion, None, "no job_completed → running");
  assert_eq!(oldest.conclusion.as_deref(), Some("success"));
  assert_eq!(oldest.job_id, "0e9d8c7b-3333-4444-8555-666677778888");
  assert_eq!(oldest.job_name.as_deref(), Some("build"));
  assert!(!oldest.started.is_empty());
  Ok(())
}
