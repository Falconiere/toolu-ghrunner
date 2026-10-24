//! `run --all`: one process supervises every registration under the runner
//! home (#208) — what the machine unit runs. It holds `<home>/supervisor.lock`
//! and drives one [`RunLoop`] per registration, each with its own `.lock`,
//! backoff, re-mint and ephemeral rules. SIGHUP rescans: new registrations
//! start, a pending removal (or vanished config) is drained, a stopped loop
//! restarts. A fatal error ends only its loop and is recorded in
//! `<data_dir>/.stopped`; SIGINT/SIGTERM cancel every loop and exit 0. A lock
//! another process holds is retried every 5 s, never driven twice.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use config::config::load_config;
use config::lockfile::{self, LockGuard};
use config::registry::{self, RegistrationEntry};
use config::stop_marker::{self, StopMarker};
use shared::{RunnerError, SecretMasker};
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::run_cmd::{self, LoopInputs, RunLoop};
use crate::supervisor_signal;

/// How often a loop retries a registration lock another process holds.
const LOCK_RETRY: Duration = Duration::from_secs(5);

/// `run --all`: supervise every registration until SIGINT/SIGTERM.
///
/// # Errors
///
/// Startup only: tracing init, the SIGHUP handler, or the supervisor lock
/// (another live `run --all` holds it, or it cannot be written).
pub(crate) async fn cmd_run_all() -> Result<(), Box<dyn std::error::Error>> {
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  crate::init_tracing_for(&masker).map_err(|e| format!("startup init: {e}"))?;
  // Installed before the supervisor lock makes this pid findable, so a
  // `register` racing the startup never hits SIGHUP's default (terminate).
  let mut hangup =
    signal(SignalKind::hangup()).map_err(|e| format!("cannot listen for SIGHUP: {e}"))?;
  let home = registry::runner_home();
  let lock_home = home.clone();
  let _supervisor_lock = off_runtime(move || acquire_supervisor_lock(&lock_home)).await??;
  let cancel = CancellationToken::new();
  run_cmd::spawn_signal_bridge(cancel.clone());
  tracing::info!(home = %home.display(), "run --all: supervising every registration (SIGHUP rescans)");

  let mut supervisor = Supervisor {
    home,
    masker,
    cancel,
    loops: BTreeMap::new(),
    retired: Vec::new(),
  };
  supervisor.rescan().await;
  supervisor.serve(&mut hangup).await;
  supervisor.shutdown().await;
  Ok(())
}

/// Take `<home>/supervisor.lock`; a live holder is another supervisor.
fn acquire_supervisor_lock(home: &Path) -> Result<LockGuard, String> {
  match lockfile::acquire(&supervisor_signal::lock_path(home), home) {
    Ok(guard) => Ok(guard),
    Err(RunnerError::LockHeld {
      pid, started_at, ..
    }) => Err(format!(
      "another `toolu-runner run --all` (pid {pid}, since {started_at}) already supervises {}",
      home.display()
    )),
    Err(e) => Err(format!("cannot take the supervisor lock: {e}")),
  }
}

/// The running registration loops, keyed by `config.toml` path.
struct Supervisor {
  home: PathBuf,
  masker: Arc<Mutex<SecretMasker>>,
  cancel: CancellationToken,
  loops: BTreeMap<PathBuf, Supervised>,
  /// Drained loops a successor replaced (the registration was registered
  /// again while its old loop finished a job); awaited at shutdown.
  retired: Vec<JoinHandle<()>>,
}

/// One registration's loop task and the token that drains it.
struct Supervised {
  drain: CancellationToken,
  task: JoinHandle<()>,
}

impl Supervisor {
  /// Rescan on every SIGHUP until SIGINT/SIGTERM.
  async fn serve(&mut self, hangup: &mut Signal) {
    loop {
      tokio::select! {
        () = self.cancel.cancelled() => return,
        received = hangup.recv() => {
          if received.is_none() {
            return self.cancel.cancelled().await;
          }
          tracing::info!("SIGHUP: rescanning registrations");
          self.rescan().await;
        },
      }
    }
  }

  /// Reconcile the loops with the registrations on disk (see the module
  /// doc). The disk is read off the async workers the job loops share; an
  /// unreadable registry keeps every running loop untouched.
  async fn rescan(&mut self) {
    self
      .loops
      .retain(|_, supervised| !supervised.task.is_finished());
    self.retired.retain(|task| !task.is_finished());
    let Some(scan) = self.read_scan().await else {
      return;
    };
    for config_path in &scan.departing {
      if let Some(supervised) = self.loops.get(config_path) {
        supervised.drain.cancel();
      }
    }
    self.start_missing(scan.entries);
    tracing::info!(registrations = self.loops.len(), "supervising");
  }

  /// Read the registrations and the live loops' state off the async
  /// workers; `None` (logged) when the registry cannot be listed.
  async fn read_scan(&self) -> Option<Scan> {
    let home = self.home.clone();
    let live: Vec<PathBuf> = self.loops.keys().cloned().collect();
    match off_runtime(move || Scan::read(&home, &live)).await {
      Ok(Ok(scan)) => Some(scan),
      Ok(Err(e)) => {
        tracing::error!(error = %e, "cannot list registrations; the running loops are kept");
        None
      },
      Err(e) => {
        tracing::error!(error = %e, "rescan failed; the running loops are kept");
        None
      },
    }
  }

  /// Start a loop for every registration without one, unless its removal
  /// is pending. A registration whose loop is still draining (its config
  /// vanished mid-job, then `register` wrote it again) gets a successor that
  /// waits for the old loop's lock.
  fn start_missing(&mut self, entries: Vec<(RegistrationEntry, bool)>) {
    for (entry, pending) in entries {
      if pending {
        tracing::info!(registration = %entry.scope.label(), "removal pending; not starting it");
        continue;
      }
      match self.loops.remove(&entry.config_path) {
        Some(live) if !live.drain.is_cancelled() => {
          self.loops.insert(entry.config_path, live);
          continue;
        },
        Some(departing) => self.retired.push(departing.task),
        None => {},
      }
      self.spawn(entry);
    }
  }

  /// Start the loop task for `entry`.
  fn spawn(&mut self, entry: RegistrationEntry) {
    let drain = CancellationToken::new();
    let span = tracing::info_span!("registration", registration = %entry.scope.label());
    let task = tokio::spawn(
      supervise(
        entry.config_path.clone(),
        Arc::clone(&self.masker),
        self.cancel.clone(),
        drain.clone(),
      )
      .instrument(span),
    );
    self
      .loops
      .insert(entry.config_path, Supervised { drain, task });
  }

  /// Wait for every loop to end (they observe the cancelled root token).
  async fn shutdown(self) {
    let live = self.loops.into_values().map(|supervised| supervised.task);
    for task in live.chain(self.retired) {
      if let Err(e) = task.await {
        tracing::error!(error = %e, "a registration loop task failed");
      }
    }
  }
}

/// One registration's whole life under the supervisor: its outcome is
/// logged, and a fatal one recorded in `.stopped`.
async fn supervise(
  config_path: PathBuf,
  masker: Arc<Mutex<SecretMasker>>,
  cancel: CancellationToken,
  drain: CancellationToken,
) {
  let error = match drive_registration(&config_path, &masker, cancel, drain).await {
    Ok(()) => return tracing::info!("registration loop ended"),
    Err(e) => e.to_string(),
  };
  if let Err(e) = off_runtime(move || record_stop(&config_path, &masker, &error)).await {
    tracing::error!(error = %e, "could not record why the registration stopped");
  }
}

/// Run blocking file work (config, lock, `.stopped`) off the async workers
/// that every registration's loop shares, inside the caller's tracing span.
pub(crate) async fn off_runtime<T: Send + 'static>(
  work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
  let span = tracing::Span::current();
  tokio::task::spawn_blocking(move || span.in_scope(work))
    .await
    .map_err(|e| format!("blocking task failed: {e}"))
}

/// Take the registration's lock (waiting out another holder), clear its
/// `.stopped`, and drive its [`RunLoop`].
async fn drive_registration(
  config_path: &Path,
  masker: &Arc<Mutex<SecretMasker>>,
  cancel: CancellationToken,
  drain: CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
  let path = config_path.to_owned();
  let (cfg, lock_path) = off_runtime(move || {
    let cfg = run_cmd::load_run_config(&path).map_err(|e| e.to_string())?;
    let lock_path = run_cmd::registration_lock_path(&cfg).map_err(|e| e.to_string())?;
    Ok::<_, String>((cfg, lock_path))
  })
  .await??;
  let Some(_lock) = acquire_when_free(&lock_path, config_path, &cancel, &drain).await? else {
    return Ok(());
  };
  let (path, lock) = (config_path.to_owned(), lock_path.clone());
  if !off_runtime(move || ready_to_drive(&path, &lock)).await? {
    return Ok(());
  }
  let run_loop = RunLoop::for_registration(LoopInputs {
    config_path: config_path.to_owned(),
    cfg: &cfg,
    masker: Arc::clone(masker),
    cancel,
    drain,
    once: false,
  })
  .map_err(|e| e.to_string())?;
  run_loop.drive().await.map_err(|e| e.to_string().into())
  // `_lock` drops here, releasing the registration.
}

/// With the lock held: `false` when a removal was requested while this loop
/// waited for it; otherwise clear the previous `.stopped` record.
fn ready_to_drive(config_path: &Path, lock_path: &Path) -> bool {
  if pending_remove(config_path) {
    tracing::info!("removal pending; not starting it");
    return false;
  }
  clear_stop_record(config_path, lock_path);
  true
}

/// Clear the registration's `.stopped` as its loop (re)starts.
fn clear_stop_record(config_path: &Path, lock_path: &Path) {
  if let Err(e) = stop_marker::clear(&data_dir_of(config_path)) {
    tracing::warn!(error = %e, "could not clear the previous .stopped record");
  }
  tracing::info!(path = %lock_path.display(), "acquired single-job lock");
}

/// Take `lock_path`, retrying every [`LOCK_RETRY`] while another live
/// process holds it (one WARN). `None` when cancelled or drained first.
async fn acquire_when_free(
  lock_path: &Path,
  config_path: &Path,
  cancel: &CancellationToken,
  drain: &CancellationToken,
) -> Result<Option<LockGuard>, String> {
  let mut warned = false;
  loop {
    let (lock, config) = (lock_path.to_owned(), config_path.to_owned());
    match off_runtime(move || lockfile::acquire(&lock, &config)).await? {
      Ok(guard) => return Ok(Some(guard)),
      Err(RunnerError::LockHeld { pid, .. }) if !warned => {
        tracing::warn!(
          pid,
          "the registration's job lock is held by another process (an older unit still \
           shutting down, or a foreground `run`); retrying every 5 s"
        );
        warned = true;
      },
      Err(RunnerError::LockHeld { .. }) => {},
      Err(e) => {
        return Err(format!(
          "cannot take the job lock {}: {e}",
          lock_path.display()
        ));
      },
    }
    tokio::select! {
      () = cancel.cancelled() => return Ok(None),
      () = drain.cancelled() => return Ok(None),
      () = tokio::time::sleep(LOCK_RETRY) => {},
    }
  }
}

/// Log a loop's fatal error and record it in `<data_dir>/.stopped`
/// (masked). A registration whose config is gone (`remove --force`) ended
/// on purpose: nothing is recorded.
fn record_stop(config_path: &Path, masker: &Mutex<SecretMasker>, error: &str) {
  if !config_path.exists() {
    tracing::info!(
      error,
      "registration loop ended after its config was removed"
    );
    return;
  }
  // A TOML parse error quotes the offending source line, which can hold a
  // secret the masker never learned (the file that names it did not load):
  // keep only what `doctor` would share.
  let shareable = crate::doctor_registrations::without_quoted_strings(
    &crate::doctor_registrations::without_source_excerpt(error),
  );
  let error = masked(masker, &shareable);
  log_stopped(&error);
  write_stop_marker(&data_dir_of(config_path), error);
}

/// The ERROR line for a stopped registration (inside its span).
fn log_stopped(error: &str) {
  tracing::error!(
    error,
    "registration stopped; the other registrations keep running — fix the cause, then reload \
     the service (SIGHUP) to restart it"
  );
}

/// Write `<data_dir>/.stopped`; a failure is only logged as a warning.
fn write_stop_marker(data_dir: &Path, error: String) {
  let record = StopMarker {
    stopped_at: chrono::Utc::now().to_rfc3339(),
    error,
  };
  if let Err(e) = stop_marker::write(data_dir, &record) {
    tracing::warn!(error = %e, "could not record why the registration stopped");
  }
}

/// `text` with every secret the masker knows replaced.
fn masked(masker: &Mutex<SecretMasker>, text: &str) -> String {
  match masker.lock() {
    Ok(guard) => guard.mask(text).into_owned(),
    Err(poisoned) => poisoned.into_inner().mask(text).into_owned(),
  }
}

/// What a rescan reads from disk, off the async workers.
struct Scan {
  /// Every registration, with whether its removal is pending.
  entries: Vec<(RegistrationEntry, bool)>,
  /// Live loops whose removal is pending or whose config is gone.
  departing: Vec<PathBuf>,
}

impl Scan {
  /// List the registrations under `home` and check the `live` loops.
  fn read(home: &Path, live: &[PathBuf]) -> Result<Self, RunnerError> {
    let entries = registry::list_registrations(home)?
      .into_iter()
      .map(|entry| {
        let pending = pending_remove(&entry.config_path);
        (entry, pending)
      })
      .collect();
    let departing = live
      .iter()
      .filter(|config_path| !config_path.exists() || pending_remove(config_path))
      .cloned()
      .collect();
    Ok(Self { entries, departing })
  }
}

/// Whether the registration's `.pending_remove` marker exists.
fn pending_remove(config_path: &Path) -> bool {
  data_dir_of(config_path).join(".pending_remove").exists()
}

/// The registration's data dir, `~` expanded but never created: its
/// configured `data_dir`, or the config's own dir when the config does not
/// load (where `register` always puts it).
fn data_dir_of(config_path: &Path) -> PathBuf {
  load_config(config_path).map_or_else(
    |_| {
      config_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
    },
    |cfg| shared::paths::expand_tilde(Path::new(&cfg.runtime.data_dir)),
  )
}
