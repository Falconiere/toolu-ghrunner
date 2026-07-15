//! `setup` subcommand: the full-screen guided first-run wizard.
//!
//! Resolves the registration inputs (github.com only), spawns the async
//! [`crate::wizard_steps::run_pipeline`] that does the real auth → register →
//! install → verify work, and drives a ratatui render loop that folds the
//! pipeline's [`StepEvent`]s into a [`WizardState`] through the pure
//! `observability::wizard` reducers. A non-interactive terminal is rejected
//! up front (pointing at the scriptable `login` / `register` /
//! `install-service` path); a RAII terminal guard restores the screen on
//! every exit path — normal return, `?`, or panic-unwind.

use std::io::{self, IsTerminal};
use std::time::Duration;

use config::auth_store::AuthStore;
use config::registry;
use crossterm::event::{self, Event, KeyEvent};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use observability::wizard::input::Action;
use observability::wizard::{self, SetupInputs, StepEvent, StepId, WizardState};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use shared::RunnerError;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;

use crate::cli::{
  SetupArgs, credentials_path_for, default_labels, runner_name_or_hostname, work_folder_or_default,
};
use crate::register_cmd;
use crate::wizard_steps::{self, SetupPlan};

/// Render-loop tick: the cap on how long input polling blocks per frame.
const TICK: Duration = Duration::from_millis(200);

/// `setup`: resolve the registration inputs, then drive the guided wizard.
///
/// Refuses a non-interactive terminal before any terminal setup (naming the
/// scriptable alternative), resolves the github.com repository + config
/// paths, decides which stages are already satisfied, and hands off to the
/// TUI driver.
pub(crate) async fn cmd_setup(args: SetupArgs) -> Result<(), Box<dyn std::error::Error>> {
  if !(io::stderr().is_terminal() && io::stdin().is_terminal()) {
    return Err(
      RunnerError::Config(
        "`setup` needs an interactive terminal. Run `toolu-runner login`, then `register`, then \
       `install-service` instead."
          .to_owned(),
      )
      .into(),
    );
  }

  let inputs = resolve_inputs(&args)?;
  let home = registry::runner_home();
  // Skip auth only when a stored login token exists AND neither the --token
  // flag nor TOOLU_RUNNER_TOKEN would override it (those force a real auth
  // pass so the caller's explicit token wins).
  let skip_auth = AuthStore::new(&home)
    .load(&inputs.host)
    .ok()
    .flatten()
    .is_some()
    && args.token.is_none()
    && std::env::var("TOOLU_RUNNER_TOKEN").is_err();
  let skip_register = inputs.config_path.exists();

  run_wizard(inputs, args.token, args.client_id, skip_auth, skip_register).await
}

/// Resolve the wizard's [`SetupInputs`] from the CLI args. `--url` absent
/// infers the repo from the cwd git remote `origin`; a non-github.com host is
/// rejected (GHES uses `register --url`). Config/credentials paths, name,
/// labels, and work folder mirror `register`'s resolution.
fn resolve_inputs(args: &SetupArgs) -> Result<SetupInputs, Box<dyn std::error::Error>> {
  let (url, host) = register_cmd::resolve_url_and_host(args.url.clone())?;
  if host != "github.com" {
    return Err(
      RunnerError::Config(
        "`setup` supports github.com only; use `register --url …` for GHES/enterprise.".to_owned(),
      )
      .into(),
    );
  }
  let (owner, repo) = register_cmd::owner_repo_from_url(&url).ok_or_else(|| {
    RunnerError::Config(format!(
      "could not parse owner/repo from the repository URL: {url}"
    ))
  })?;

  let home = registry::runner_home();
  let config_path = match args.config.clone() {
    Some(path) => path,
    None => register_cmd::register_config_path(&url, &home)?,
  };
  let creds_path = credentials_path_for(&config_path);
  let labels = if args.labels.is_empty() {
    default_labels()
  } else {
    args.labels.clone()
  };

  Ok(SetupInputs {
    host,
    url,
    owner,
    repo,
    name: runner_name_or_hostname(args.name.clone()),
    labels,
    runner_group: "Default".to_owned(),
    work_folder: work_folder_or_default(None),
    config_path,
    creds_path,
  })
}

/// Spawn the async pipeline and drive the render loop under a terminal guard.
/// Seeds the initial paint from the driver's OWN `skip_auth` / `skip_register`
/// decisions (with the same reasons the pipeline emits) so the paint never
/// diverges from the stages the pipeline actually skips; the guard restores
/// the screen when this function returns.
async fn run_wizard(
  inputs: SetupInputs,
  token: Option<String>,
  client_id: Option<String>,
  skip_auth: bool,
  skip_register: bool,
) -> Result<(), Box<dyn std::error::Error>> {
  let mut state = WizardState::new(inputs.clone());
  // Pre-skip exactly what the pipeline will pre-skip. Building these from the
  // driver's own bools (not a separate on-disk probe) keeps the initial paint
  // in lock-step with the pipeline: Auth reuses a stored login token, Register
  // reuses an existing registration.
  if skip_auth {
    state.apply(StepEvent::Skipped {
      step: StepId::Auth,
      reason: "using stored login token".to_owned(),
    });
  }
  if skip_register {
    state.apply(StepEvent::Skipped {
      step: StepId::Register,
      reason: "already registered".to_owned(),
    });
  }

  let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
  let cancel = CancellationToken::new();
  let plan = SetupPlan {
    inputs,
    flag_token: token,
    client_id,
    skip_auth,
    skip_register,
  };
  tokio::spawn(wizard_steps::run_pipeline(plan, tx, cancel.clone()));

  let _guard = TerminalGuard::enter()?;
  let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
  render_loop(&mut terminal, &mut state, &mut rx, &cancel)
}

/// Fold pipeline events into `state` and paint in two explicit phases. The
/// interactive phase (`while !finished`) draws, drains every [`StepEvent`],
/// recomputes `finished`, and lets a `Quit` key cancel the pipeline. Once the
/// pipeline finished (not on a mid-run quit) the final-frame hold draws the
/// last frame ONCE and blocks for a key, so the outcome stays on screen. A
/// failed stage maps to `Err` (non-zero exit); a clean finish or quit is `Ok`.
fn render_loop(
  terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
  state: &mut WizardState,
  rx: &mut UnboundedReceiver<StepEvent>,
  cancel: &CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
  let mut finished = false;
  while !finished {
    terminal.draw(|frame| wizard::ui::render(frame, state))?;
    while let Ok(ev) = rx.try_recv() {
      state.apply(ev);
    }
    finished = state.done || state.error.is_some();
    if finished {
      break;
    }
    if let Some(key) = poll_key()?
      && wizard::input::action_for(state, key) == Action::Quit
    {
      cancel.cancel();
      break;
    }
  }

  // Only hold the final frame when the pipeline actually finished — a mid-run
  // quit (finished == false) skips straight to the outcome and exits.
  if finished {
    while let Ok(ev) = rx.try_recv() {
      state.apply(ev);
    }
    terminal.draw(|frame| wizard::ui::render(frame, state))?;
    block_for_key()?;
  }

  match &state.error {
    Some(error) => Err(RunnerError::StepExecution(error.clone()).into()),
    None => Ok(()),
  }
}

/// Poll for a key event for up to [`TICK`]: `Some(key)` on a key press, `None`
/// on a timeout or a non-key event (resize / mouse / paste). Split out so the
/// render loop reads a single `Option<KeyEvent>` per frame.
fn poll_key() -> io::Result<Option<KeyEvent>> {
  if !event::poll(TICK)? {
    return Ok(None);
  }
  let event = event::read()?;
  if let Event::Key(key) = event {
    Ok(Some(key))
  } else {
    Ok(None)
  }
}

/// Block until the user presses a key, ignoring timeouts and non-key events.
/// Holds the wizard's final frame on screen until it is dismissed. Reuses
/// [`poll_key`], so a non-key event or a [`TICK`] timeout just polls again.
fn block_for_key() -> io::Result<()> {
  loop {
    if poll_key()?.is_some() {
      return Ok(());
    }
  }
}

/// RAII guard for the wizard's terminal state. [`TerminalGuard::enter`]
/// enables raw mode and switches to the alternate screen; `Drop` restores
/// both — so a normal return, an early `?`, or a panic-unwind all leave the
/// user's terminal intact.
struct TerminalGuard;

impl TerminalGuard {
  /// Enter raw mode + the alternate screen. On failure the partial state is
  /// torn down before returning the error.
  fn enter() -> io::Result<Self> {
    enable_raw_mode()?;
    if let Err(e) = wizard::term::enter_terminal(&mut io::stdout()) {
      let _ = disable_raw_mode();
      return Err(e);
    }
    Ok(Self)
  }
}

impl Drop for TerminalGuard {
  fn drop(&mut self) {
    let mut out = io::stdout();
    let _ = wizard::term::leave_terminal(&mut out);
    let _ = disable_raw_mode();
  }
}
