//! Render smoke test: the real fixture reduced through `watch::state`
//! draws on ratatui's `TestBackend` with the expected job, step, badge,
//! and log content visible.

use std::error::Error;
use std::path::PathBuf;

use observability::journal::JournalLine;
use observability::journal::scan_jobs;
use observability::watch::state::{App, OpenJob};
use observability::watch::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

const FIXTURE: &str = include_str!("fixtures/journal/canonical.jsonl");

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// App with the fixture scanned into the job list and opened in detail.
fn fixture_app() -> TestResult<(App, tempfile::TempDir)> {
  let dir = tempfile::tempdir()?;
  std::fs::write(dir.path().join("20260708T210044Z-fix.jsonl"), FIXTURE)?;

  let mut app = App::new("test-runner".to_owned());
  app.lock_line = "idle".to_owned();
  app.set_jobs(scan_jobs(dir.path())?);

  let mut job = OpenJob::new(PathBuf::from("fixture.jsonl"));
  for raw in FIXTURE.lines() {
    job.apply(serde_json::from_str::<JournalLine>(raw)?);
  }
  app.opened = Some(job);
  Ok((app, dir))
}

#[test]
fn fixture_renders_expected_content() -> TestResult {
  let (app, _dir) = fixture_app()?;
  let mut terminal = Terminal::new(TestBackend::new(120, 30))?;
  terminal.draw(|f| ui::render(f, &app))?;

  let text: String = terminal
    .backend()
    .buffer()
    .content()
    .iter()
    .map(ratatui::buffer::Cell::symbol)
    .collect();

  for needle in [
    "toolu-runner watch",
    "test-runner",
    "build",
    "Run echo hello from step one",
    "Run echo done from step two",
    "[warning: deprecated feature]",
    "✓",
    "hello from step one",
    "success",
  ] {
    assert!(text.contains(needle), "rendered frame missing {needle:?}");
  }
  Ok(())
}

#[test]
fn confirm_prompt_renders_in_footer() -> TestResult {
  let (mut app, _dir) = fixture_app()?;
  app.confirm_cancel = true;
  let mut terminal = Terminal::new(TestBackend::new(120, 30))?;
  terminal.draw(|f| ui::render(f, &app))?;
  let text: String = terminal
    .backend()
    .buffer()
    .content()
    .iter()
    .map(ratatui::buffer::Cell::symbol)
    .collect();
  assert!(text.contains("cancel the running job"));
  Ok(())
}
