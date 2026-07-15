//! `wizard::state` reducer over REAL `StepEvent` sequences: a full
//! four-stage run completes (AC-2), a mid-run `Failed` halts advancement
//! and blocks later stages (AC-3), and `probe_skips` skips Auth + Register
//! from real on-disk state (AC-5, AC-13).

use std::error::Error;
use std::path::PathBuf;

use config::auth_store::{AuthStore, StoredToken};
use observability::wizard::state::{SetupInputs, StepEvent, StepId, StepStatus, WizardState};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// Realistic setup inputs for `acme/widgets`, with a caller-chosen
/// `config_path` so the register-skip probe can be steered.
fn inputs_with(config_path: PathBuf) -> SetupInputs {
  SetupInputs {
    host: "github.com".to_owned(),
    url: "https://github.com/acme/widgets".to_owned(),
    owner: "acme".to_owned(),
    repo: "widgets".to_owned(),
    name: "runner-1".to_owned(),
    labels: vec!["self-hosted".to_owned(), "linux".to_owned()],
    runner_group: "default".to_owned(),
    work_folder: "_work".to_owned(),
    config_path,
    creds_path: PathBuf::from("/tmp/creds.json"),
  }
}

/// The Started→Done pair a stage emits on a clean run.
fn ran(step: StepId, summary: &str) -> [StepEvent; 2] {
  [
    StepEvent::Started(step),
    StepEvent::Done {
      step,
      summary: summary.to_owned(),
    },
  ]
}

#[test]
fn full_run_marks_every_step_done() {
  // AC-2: a real four-stage run leaves all steps Done, done set, no error.
  let mut state = WizardState::new(inputs_with(PathBuf::from("/does/not/exist.toml")));
  let sequence: Vec<StepEvent> = [
    ran(StepId::Auth, "logged in as octocat"),
    ran(StepId::Register, "registered runner-1 for acme/widgets"),
    ran(
      StepId::Install,
      "launchd agent io.toolu.runner.acme.widgets active",
    ),
    ran(StepId::Verify, "runner online (long-polling for jobs)"),
  ]
  .concat();

  for ev in sequence {
    state.apply(ev);
  }

  for id in StepId::all() {
    assert_eq!(state.status(id), StepStatus::Done, "{id:?} must be Done");
  }
  assert!(state.done, "all-terminal run must set done");
  assert!(state.error.is_none(), "a clean run carries no error");
}

#[test]
fn failed_step_halts_and_blocks_later_steps() {
  // AC-3: a Failed Register sets the error, leaves Install/Verify Pending,
  // and no later event can push a step to Active or Done.
  let mut state = WizardState::new(inputs_with(PathBuf::from("/does/not/exist.toml")));
  for ev in ran(StepId::Auth, "logged in as octocat") {
    state.apply(ev);
  }
  state.apply(StepEvent::Started(StepId::Register));
  state.apply(StepEvent::Failed {
    step: StepId::Register,
    error: "generate-jitconfig returned 403 Forbidden".to_owned(),
  });

  assert_eq!(state.status(StepId::Auth), StepStatus::Done);
  assert_eq!(state.status(StepId::Register), StepStatus::Failed);
  assert_eq!(state.status(StepId::Install), StepStatus::Pending);
  assert_eq!(state.status(StepId::Verify), StepStatus::Pending);
  assert_eq!(
    state.error.as_deref(),
    Some("generate-jitconfig returned 403 Forbidden")
  );
  assert!(!state.done, "a failed run is not done");

  // Later events must not resurrect progress after the failure.
  state.apply(StepEvent::Started(StepId::Install));
  assert_eq!(
    state.status(StepId::Install),
    StepStatus::Pending,
    "no step goes Active after an error"
  );
  state.apply(StepEvent::Done {
    step: StepId::Install,
    summary: "should be ignored".to_owned(),
  });
  assert_eq!(
    state.status(StepId::Install),
    StepStatus::Pending,
    "no step goes Done after an error"
  );
}

#[test]
fn probe_skips_auth_and_register_from_disk() -> TestResult {
  // AC-5 / AC-13: a stored token + an existing config.toml on real disk
  // pre-skip exactly Auth and Register — nothing else.
  let home = tempfile::tempdir()?;
  AuthStore::new(home.path()).save(&StoredToken {
    access_token: "gho_realdeviceflowtoken".to_owned(),
    scope: "repo".to_owned(),
    host: "github.com".to_owned(),
    issued_at: "2026-07-15T00:00:00Z".to_owned(),
  })?;
  let config_path = home.path().join("config.toml");
  std::fs::write(&config_path, "# already registered\n")?;

  let inputs = inputs_with(config_path);
  let skips = WizardState::probe_skips(home.path(), &inputs);

  let steps: Vec<StepId> = skips.iter().filter_map(skipped_step).collect();
  assert_eq!(
    steps.len(),
    skips.len(),
    "probe must only emit Skipped events"
  );
  assert_eq!(steps.len(), 2, "exactly Auth + Register skip");
  assert!(steps.contains(&StepId::Auth), "stored token skips Auth");
  assert!(
    steps.contains(&StepId::Register),
    "existing config skips Register"
  );
  assert!(!steps.contains(&StepId::Install));
  assert!(!steps.contains(&StepId::Verify));
  Ok(())
}

#[test]
fn probe_skips_nothing_on_empty_home() -> TestResult {
  // No stored token and no config.toml → no stage is pre-skipped.
  let home = tempfile::tempdir()?;
  let inputs = inputs_with(home.path().join("config.toml"));
  let skips = WizardState::probe_skips(home.path(), &inputs);
  assert!(skips.is_empty(), "clean home skips nothing, got {skips:?}");
  Ok(())
}

/// The `StepId` of a `Skipped` event, or `None` for any other variant.
fn skipped_step(ev: &StepEvent) -> Option<StepId> {
  if let StepEvent::Skipped { step, .. } = ev {
    Some(*step)
  } else {
    None
  }
}
