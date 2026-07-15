//! Pure reducer for the setup wizard: folds `StepEvent`s emitted by the
//! four setup stages (authenticate → register → install → verify) into a
//! `WizardState` the `ui` renders. No I/O and no network — the bin drives
//! the real work and feeds the outcomes here. `probe_skips` is the one
//! read-only exception: it inspects on-disk state to pre-skip stages that
//! are already satisfied.

use std::path::{Path, PathBuf};

use config::auth_store::AuthStore;

/// One stage of the guided setup, in execution order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepId {
  /// Obtain a GitHub bearer (device flow / stored token).
  Auth,
  /// Register the JIT runner for the repo.
  Register,
  /// Install the boot/crash supervisor service.
  Install,
  /// Confirm the runner came online.
  Verify,
}

impl StepId {
  /// Position of this step (`Auth` = 0 … `Verify` = 3), the array index
  /// into [`WizardState::steps`].
  pub fn idx(self) -> usize {
    match self {
      StepId::Auth => 0,
      StepId::Register => 1,
      StepId::Install => 2,
      StepId::Verify => 3,
    }
  }

  /// All four steps in execution order.
  pub fn all() -> [StepId; 4] {
    [
      StepId::Auth,
      StepId::Register,
      StepId::Install,
      StepId::Verify,
    ]
  }

  /// The step after this one, or `None` past `Verify`.
  pub fn next(self) -> Option<StepId> {
    match self {
      StepId::Auth => Some(StepId::Register),
      StepId::Register => Some(StepId::Install),
      StepId::Install => Some(StepId::Verify),
      StepId::Verify => None,
    }
  }
}

/// Lifecycle state of a single setup step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StepStatus {
  /// Not started yet.
  #[default]
  Pending,
  /// Currently running.
  Active,
  /// Finished successfully.
  Done,
  /// Skipped because it was already satisfied.
  Skipped,
  /// Failed — halts the wizard.
  Failed,
}

impl StepStatus {
  /// Whether this status counts toward completion (`Done` or `Skipped`).
  fn is_complete(self) -> bool {
    matches!(self, StepStatus::Done | StepStatus::Skipped)
  }
}

/// A progress signal from one setup stage, folded by [`WizardState::apply`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepEvent {
  /// The stage began running.
  Started(StepId),
  /// A human-readable progress line for the active stage.
  Progress {
    /// Which stage the message belongs to.
    step: StepId,
    /// The progress message.
    msg: String,
  },
  /// A GitHub device-flow code to enter in the browser.
  DeviceCode {
    /// The user code to type at the verification URL.
    user_code: String,
    /// Where to enter the code (`https://github.com/login/device`).
    verification_uri: String,
  },
  /// The stage completed successfully.
  Done {
    /// Which stage completed.
    step: StepId,
    /// A one-line summary of what it produced.
    summary: String,
  },
  /// The stage was skipped (already satisfied).
  Skipped {
    /// Which stage was skipped.
    step: StepId,
    /// Why it was skipped.
    reason: String,
  },
  /// The stage failed — no later stage runs.
  Failed {
    /// Which stage failed.
    step: StepId,
    /// The failure message.
    error: String,
  },
}

/// The resolved parameters the wizard registers a runner with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetupInputs {
  /// GitHub host (e.g. `github.com`).
  pub host: String,
  /// Repository URL the runner serves.
  pub url: String,
  /// Repository owner (user or organization).
  pub owner: String,
  /// Repository name.
  pub repo: String,
  /// Runner display name.
  pub name: String,
  /// Runner labels.
  pub labels: Vec<String>,
  /// Runner group.
  pub runner_group: String,
  /// Work folder for job checkouts.
  pub work_folder: String,
  /// Where the registration `config.toml` is written.
  pub config_path: PathBuf,
  /// Where the `credentials.json` is written.
  pub creds_path: PathBuf,
}

/// The full wizard model: per-step status plus the active stage's detail.
#[derive(Debug)]
pub struct WizardState {
  /// Per-step status, indexed by [`StepId::idx`].
  pub steps: [StepStatus; 4],
  /// The stage currently in focus.
  pub active: StepId,
  /// The resolved setup parameters.
  pub inputs: SetupInputs,
  /// A pending device-flow prompt `(user_code, verification_uri)`.
  pub device_code: Option<(String, String)>,
  /// The most recent progress/summary line.
  pub last_msg: Option<String>,
  /// The first fatal error, once any stage has failed.
  pub error: Option<String>,
  /// Whether every stage is complete (`Done` or `Skipped`).
  pub done: bool,
}

impl WizardState {
  /// Fresh state: all steps `Pending`, focus on `Auth`.
  pub fn new(inputs: SetupInputs) -> Self {
    Self {
      steps: [StepStatus::Pending; 4],
      active: StepId::Auth,
      inputs,
      device_code: None,
      last_msg: None,
      error: None,
      done: false,
    }
  }

  /// Status of `id` (`Pending` if somehow out of range — never happens
  /// for the four real steps).
  pub fn status(&self, id: StepId) -> StepStatus {
    self.steps.get(id.idx()).copied().unwrap_or_default()
  }

  /// Fold one event into the model (pure — no I/O).
  ///
  /// After a failure (`error.is_some()`) no step is marked `Active` or
  /// `Done`; `Progress` / `DeviceCode` still update the detail lines.
  pub fn apply(&mut self, ev: StepEvent) {
    match ev {
      StepEvent::Started(step) => {
        if self.error.is_none() {
          self.set_status(step, StepStatus::Active);
          self.active = step;
        }
      },
      StepEvent::Progress { msg, .. } => self.last_msg = Some(msg),
      StepEvent::DeviceCode {
        user_code,
        verification_uri,
      } => self.device_code = Some((user_code, verification_uri)),
      StepEvent::Done { step, summary } => {
        if self.error.is_none() {
          self.set_status(step, StepStatus::Done);
          self.last_msg = Some(summary);
          self.device_code = None;
          self.advance_active();
        }
      },
      StepEvent::Skipped { step, reason } => {
        self.set_status(step, StepStatus::Skipped);
        self.last_msg = Some(reason);
        self.device_code = None;
        self.advance_active();
      },
      StepEvent::Failed { step, error } => {
        self.set_status(step, StepStatus::Failed);
        self.error = Some(error);
      },
    }
    self.done = StepId::all()
      .into_iter()
      .all(|id| self.status(id).is_complete());
  }

  /// Read-only probe: emit `Skipped` events for stages already satisfied on
  /// disk. `Auth` skips when a login token is stored for `inputs.host`;
  /// `Register` skips when `inputs.config_path` already exists. A store
  /// read error is swallowed to "do not skip" — a probe failure never
  /// suppresses a stage. Never emits for `Install` / `Verify`.
  pub fn probe_skips(home: &Path, inputs: &SetupInputs) -> Vec<StepEvent> {
    let mut events = Vec::new();
    if token_stored(home, &inputs.host) {
      events.push(StepEvent::Skipped {
        step: StepId::Auth,
        reason: format!("already logged in to {}", inputs.host),
      });
    }
    if inputs.config_path.exists() {
      events.push(StepEvent::Skipped {
        step: StepId::Register,
        reason: format!(
          "registration already exists at {}",
          inputs.config_path.display()
        ),
      });
    }
    events
  }

  /// Set `id`'s status; a no-op if the index is somehow out of range.
  fn set_status(&mut self, id: StepId, status: StepStatus) {
    if let Some(slot) = self.steps.get_mut(id.idx()) {
      *slot = status;
    }
  }

  /// Move focus to the first still-`Pending` step, else leave it as-is.
  fn advance_active(&mut self) {
    for id in StepId::all() {
      if self.status(id) == StepStatus::Pending {
        self.active = id;
        return;
      }
    }
  }
}

/// Whether a login token is stored for `host` under `home`. Any store
/// error reads as "not stored" so a probe failure never forces a skip.
fn token_stored(home: &Path, host: &str) -> bool {
  AuthStore::new(home).load(host).ok().flatten().is_some()
}
