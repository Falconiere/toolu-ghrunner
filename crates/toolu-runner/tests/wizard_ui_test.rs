//! Render smoke test for the setup wizard: a real `WizardState` built from
//! constructed `StepEvent`s (including a device-code prompt) draws on
//! ratatui's `TestBackend` with the step names and the device-code detail
//! line visible. Real data, no mocks.

use std::error::Error;
use std::path::PathBuf;

use observability::wizard::state::{SetupInputs, StepEvent, StepId, WizardState};
use observability::wizard::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// Realistic setup inputs for `acme/widgets`.
fn inputs() -> SetupInputs {
  SetupInputs {
    host: "github.com".to_owned(),
    url: "https://github.com/acme/widgets".to_owned(),
    owner: "acme".to_owned(),
    repo: "widgets".to_owned(),
    name: "runner-1".to_owned(),
    labels: vec!["self-hosted".to_owned(), "linux".to_owned()],
    runner_group: "default".to_owned(),
    work_folder: "_work".to_owned(),
    config_path: PathBuf::from("/home/runner/config.toml"),
    creds_path: PathBuf::from("/home/runner/credentials.json"),
  }
}

/// The whole rendered frame as one string.
fn rendered(state: &WizardState) -> TestResult<String> {
  let mut terminal = Terminal::new(TestBackend::new(120, 30))?;
  terminal.draw(|f| ui::render(f, state))?;
  Ok(
    terminal
      .backend()
      .buffer()
      .content()
      .iter()
      .map(ratatui::buffer::Cell::symbol)
      .collect(),
  )
}

#[test]
fn frame_shows_step_names_and_device_code() -> TestResult {
  // Auth is running and has surfaced a device code; the panel must show
  // every step name and the "Enter code … at …" detail line.
  let mut state = WizardState::new(inputs());
  state.apply(StepEvent::Started(StepId::Auth));
  state.apply(StepEvent::DeviceCode {
    user_code: "WDJB-MJHT".to_owned(),
    verification_uri: "https://github.com/login/device".to_owned(),
  });

  let text = rendered(&state)?;
  for needle in [
    "toolu-runner setup",
    "Authenticate",
    "Register runner",
    "Install service",
    "Verify online",
    "Enter code WDJB-MJHT at https://github.com/login/device",
    "q / Ctrl-C to quit",
  ] {
    assert!(text.contains(needle), "rendered frame missing {needle:?}");
  }
  Ok(())
}

#[test]
fn finished_frame_shows_any_key_footer() -> TestResult {
  // A fully skipped/done run flips the footer to the dismiss hint.
  let mut state = WizardState::new(inputs());
  for step in StepId::all() {
    state.apply(StepEvent::Done {
      step,
      summary: format!("{step:?} done"),
    });
  }
  assert!(state.done, "every step Done sets done");

  let text = rendered(&state)?;
  assert!(
    text.contains("press any key to exit"),
    "finished frame must show the dismiss footer"
  );
  Ok(())
}
