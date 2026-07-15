//! Async step executors for the `setup` wizard: the real auth → register →
//! install → verify work the full-screen driver ([`crate::setup_cmd`]) spawns
//! as a background task. Each stage emits [`StepEvent`]s over an unbounded
//! channel that the driver folds into the wizard model; a failure emits
//! [`StepEvent::Failed`] and returns (no panics, no swallowed errors). The
//! pure reducers live in `observability::wizard`; this module owns the I/O.

use std::path::{Path, PathBuf};
use std::time::Duration;

use config::auth_store::{self, AuthStore, BearerDecision};
use config::registry;
use observability::wizard::verify::{VerifyOutcome, verify_decision};
use observability::wizard::{SetupInputs, StepEvent, StepId};
use shared::RunnerError;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;
use wire::net;

use crate::login_cmd;
use crate::register_cmd::{self, RegisterPersist};
use crate::service_cmd;

/// The log line the listener emits once it is long-polling for jobs; the
/// verify stage waits for it to confirm the runner came online.
const ONLINE_MARKER: &str = "long-polling for jobs";

/// How many one-second polls the verify stage waits for the online marker.
const VERIFY_POLL_SECS: usize = 15;

/// The resolved plan the pipeline runs: the setup inputs plus the auth
/// choices and the two skip decisions the driver computed. Bundled into one
/// value (like [`crate::register_cmd::RegisterPersist`]) so [`run_pipeline`]
/// takes few arguments and the spawned task owns everything it needs.
pub(crate) struct SetupPlan {
  /// The resolved registration inputs.
  pub(crate) inputs: SetupInputs,
  /// A `--token` bearer, if the caller passed one.
  pub(crate) flag_token: Option<String>,
  /// A `--client-id` override for the device flow, if any.
  pub(crate) client_id: Option<String>,
  /// Reuse the stored login token instead of authenticating.
  pub(crate) skip_auth: bool,
  /// Skip registration because a config already exists.
  pub(crate) skip_register: bool,
}

/// Drive the four setup stages in order, emitting progress over `tx`. Runs as
/// a spawned task; each stage either finishes (emitting `Done`/`Skipped`) or
/// fails (emitting `Failed` and returning — no later stage runs). `cancel`
/// aborts the verify wait early. `plan` is owned so the task runs detached
/// from the driver.
pub(crate) async fn run_pipeline(
  plan: SetupPlan,
  tx: UnboundedSender<StepEvent>,
  cancel: CancellationToken,
) {
  let SetupPlan {
    inputs,
    flag_token,
    client_id,
    skip_auth,
    skip_register,
  } = plan;

  let bearer = match run_auth(&inputs, flag_token, client_id, skip_auth, &tx).await {
    Ok(bearer) => bearer,
    Err(e) => return emit_failed(&tx, StepId::Auth, &e),
  };
  if let Err(e) = run_register(&inputs, &bearer, skip_register, &tx).await {
    return emit_failed(&tx, StepId::Register, &e);
  }
  let label = match run_install(&inputs, &tx) {
    Ok(label) => label,
    Err(e) => return emit_failed(&tx, StepId::Install, &e),
  };
  if let Err(e) = run_verify(&inputs, &label, &cancel, &tx).await {
    emit_failed(&tx, StepId::Verify, &e);
  }
}

/// Emit a `Failed` event for `step`. Send errors are ignored — the driver
/// having dropped the receiver means the wizard already exited.
fn emit_failed(tx: &UnboundedSender<StepEvent>, step: StepId, error: &RunnerError) {
  let _ = tx.send(StepEvent::Failed {
    step,
    error: error.to_string(),
  });
}

/// Resolve the GitHub bearer for registration. `skip_auth` reuses the stored
/// login token (emitting `Skipped`); otherwise the bearer resolves flag >
/// `TOOLU_RUNNER_TOKEN` env > stored token, and a missing one on this
/// (always-interactive) path starts the device flow, routing the user code
/// through a `DeviceCode` event.
async fn run_auth(
  inputs: &SetupInputs,
  flag_token: Option<String>,
  client_id: Option<String>,
  skip_auth: bool,
  tx: &UnboundedSender<StepEvent>,
) -> Result<String, RunnerError> {
  let store = AuthStore::new(&registry::runner_home());
  if skip_auth {
    let stored = store
      .load(&inputs.host)?
      .ok_or_else(|| RunnerError::Auth(format!("no stored login token for {}", inputs.host)))?;
    let _ = tx.send(StepEvent::Skipped {
      step: StepId::Auth,
      reason: "using stored login token".to_owned(),
    });
    return Ok(stored.access_token);
  }

  let _ = tx.send(StepEvent::Started(StepId::Auth));
  let resolved = auth_store::resolve_bearer(&store, &inputs.host, flag_token)?;
  match auth_store::decide_bearer(resolved, true) {
    BearerDecision::Use(token) => {
      let _ = tx.send(StepEvent::Done {
        step: StepId::Auth,
        summary: "authenticated".to_owned(),
      });
      Ok(token)
    },
    BearerDecision::StartDeviceFlow => {
      let ptx = tx.clone();
      let present = move |dc: &net::device_auth::DeviceCodeResponse| {
        let _ = ptx.send(StepEvent::DeviceCode {
          user_code: dc.user_code.clone(),
          verification_uri: dc.verification_uri.clone(),
        });
      };
      let stored = login_cmd::run_device_flow(&inputs.host, client_id, &store, present)
        .await
        .map_err(|e| RunnerError::Auth(e.to_string()))?;
      let _ = tx.send(StepEvent::Done {
        step: StepId::Auth,
        summary: "logged in via device flow".to_owned(),
      });
      Ok(stored.access_token)
    },
    BearerDecision::Fail(msg) => Err(RunnerError::Auth(msg)),
  }
}

/// Register the runner via `generate-jitconfig` and persist the config +
/// credentials. `skip_register` (an existing registration) emits `Skipped`.
async fn run_register(
  inputs: &SetupInputs,
  bearer: &str,
  skip_register: bool,
  tx: &UnboundedSender<StepEvent>,
) -> Result<(), RunnerError> {
  if skip_register {
    let _ = tx.send(StepEvent::Skipped {
      step: StepId::Register,
      reason: "already registered".to_owned(),
    });
    return Ok(());
  }
  let _ = tx.send(StepEvent::Started(StepId::Register));
  let runner_id = register_cmd::register_and_persist(RegisterPersist {
    url: &inputs.url,
    token: bearer,
    runner_name: &inputs.name,
    labels: &inputs.labels,
    runner_group: &inputs.runner_group,
    work_folder: &inputs.work_folder,
    host: &inputs.host,
    config_path: &inputs.config_path,
    creds_path: &inputs.creds_path,
    replace: false,
  })
  .await?;
  let _ = tx.send(StepEvent::Done {
    step: StepId::Register,
    summary: format!("runner #{runner_id}"),
  });
  Ok(())
}

/// Install (and activate) the boot/crash supervisor unit for the
/// registration. Idempotent — no skip is needed. Returns the service label
/// so the verify stage can query the supervisor.
fn run_install(
  inputs: &SetupInputs,
  tx: &UnboundedSender<StepEvent>,
) -> Result<String, RunnerError> {
  let _ = tx.send(StepEvent::Started(StepId::Install));
  let out = service_cmd::install_service_core(&inputs.config_path, false, false, false)?;
  let label = out.label.clone();
  let _ = tx.send(StepEvent::Done {
    step: StepId::Install,
    summary: out.label,
  });
  Ok(label)
}

/// Confirm the runner came online: the supervisor service must be active AND
/// the runner log must carry the online marker. `Unconfirmed` is reported as
/// a successful `Done` (not a failure) pointing the user at `watch`.
async fn run_verify(
  inputs: &SetupInputs,
  label: &str,
  cancel: &CancellationToken,
  tx: &UnboundedSender<StepEvent>,
) -> Result<(), RunnerError> {
  let _ = tx.send(StepEvent::Started(StepId::Verify));
  let summary = match verify_online(inputs, label, cancel).await {
    VerifyOutcome::Online => "runner online".to_owned(),
    VerifyOutcome::Unconfirmed(reason) => {
      format!("unconfirmed ({reason}) — run `toolu-runner watch`")
    },
  };
  let _ = tx.send(StepEvent::Done {
    step: StepId::Verify,
    summary,
  });
  Ok(())
}

/// Gather the two verify facts and decide. Polls the runner log up to
/// [`VERIFY_POLL_SECS`] seconds for the online marker (cancel-aware), then
/// checks the supervisor once and folds both through [`verify_decision`].
async fn verify_online(
  inputs: &SetupInputs,
  label: &str,
  cancel: &CancellationToken,
) -> VerifyOutcome {
  let diag_dirs = candidate_diag_dirs(inputs);
  let mut log_tail = String::new();
  for i in 0..VERIFY_POLL_SECS {
    if cancel.is_cancelled() {
      break;
    }
    // Scan every candidate dir each poll; the marker counts if it appears in
    // ANY of them (concatenating the per-dir `runner.log*` tails).
    log_tail = diag_dirs.iter().map(|d| read_runner_log_tail(d)).collect();
    if log_tail.contains(ONLINE_MARKER) {
      break;
    }
    if i + 1 < VERIFY_POLL_SECS {
      tokio::time::sleep(Duration::from_secs(1)).await;
    }
  }
  verify_decision(service_active(label, inputs), &log_tail, ONLINE_MARKER)
}

/// The deduplicated `_diag` dirs the verify stage scans for the online marker.
/// The runner writes its tracing log to the fixed home `_diag/` (tracing
/// inits before per-repo config loads); we also scan the registration dir's
/// `_diag/` as a fallback so verify is robust to custom setups.
fn candidate_diag_dirs(inputs: &SetupInputs) -> Vec<PathBuf> {
  let mut dirs = vec![shared::startup::default_data_dir().join("_diag")];
  if let Some(parent) = inputs.config_path.parent() {
    let reg_diag = parent.join("_diag");
    if !dirs.contains(&reg_diag) {
      dirs.push(reg_diag);
    }
  }
  dirs
}

/// Whether the supervisor reports the runner's unit as loaded/active. macOS
/// asks launchd (`launchctl list <label>`, exit 0 = loaded); Linux asks
/// systemd (`systemctl --user is-active <unit>`). Any spawn error or non-zero
/// exit reads as "not active" so a probe failure never blocks the wizard.
fn service_active(label: &str, inputs: &SetupInputs) -> bool {
  if cfg!(target_os = "macos") {
    command_succeeds("launchctl", &["list", label])
  } else if cfg!(target_os = "linux") {
    let unit = format!("toolu-runner-{}-{}.service", inputs.owner, inputs.repo);
    command_succeeds("systemctl", &["--user", "is-active", &unit])
  } else {
    false
  }
}

/// Run `program args` and report whether it spawned and exited zero.
fn command_succeeds(program: &str, args: &[&str]) -> bool {
  std::process::Command::new(program)
    .args(args)
    .output()
    .is_ok_and(|o| o.status.success())
}

/// Concatenated tails of every `runner.log*` file in `diag` (the daily
/// rotator names them `runner.log.<date>`). Each file's last 64 KiB is read;
/// an unreadable dir or file contributes nothing (the marker just stays
/// unseen, which reads as `Unconfirmed`, not a failure).
fn read_runner_log_tail(diag: &Path) -> String {
  let Ok(entries) = std::fs::read_dir(diag) else {
    return String::new();
  };
  let mut files: Vec<std::path::PathBuf> = entries
    .flatten()
    .map(|e| e.path())
    .filter(|p| {
      p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("runner.log"))
    })
    .collect();
  files.sort();
  let mut tail = String::new();
  for file in &files {
    if let Some(chunk) = read_tail(file, 64 * 1024) {
      tail.push_str(&chunk);
    }
  }
  tail
}

/// The last `max` bytes of `path` as a lossy string, or `None` if it cannot
/// be read. Reads the whole (small) log then slices — no seeking.
fn read_tail(path: &Path, max: usize) -> Option<String> {
  let bytes = std::fs::read(path).ok()?;
  let start = bytes.len().saturating_sub(max);
  let slice = bytes.get(start..).unwrap_or_default();
  Some(String::from_utf8_lossy(slice).into_owned())
}
