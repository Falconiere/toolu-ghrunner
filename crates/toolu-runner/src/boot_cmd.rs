//! `boot` subcommand: zero-config one-shot container mode.
//!
//! The image ENTRYPOINT is `["toolu-runner", "boot"]` — no argv, only
//! `TOOLU_JITCONFIG` (required) and `TOOLU_DEADLINE` (optional watchdog
//! backstop) in the environment. Runs one JIT lifecycle with no persisted
//! state (no `config.toml`/`credentials.json`/`.lock`/auth store/re-mint)
//! and returns an explicit exit code for `main` to apply (see
//! [`cmd_boot`]). Bin-only crate, no lib target: pure helpers here are
//! pinned by the shell-out tests in `tests/boot_cmd_test.rs`, not inline
//! unit tests.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use listener::GitHubListener;
use shared::{CacheConfig, Conclusion, RunnerConfig, RunnerError, SecretMasker, ServicesMode};
use tokio_util::sync::CancellationToken;

/// Grace period after the deadline fires before the watchdog force-exits:
/// long enough for a graceful cancel to report `Cancelled` to GitHub, short
/// enough that a provider's own deadline enforcement never races us.
const WATCHDOG_GRACE: Duration = Duration::from_secs(30);

/// `boot`: read `TOOLU_JITCONFIG` / `TOOLU_DEADLINE`, run one JIT lifecycle,
/// return the process exit code: `0` Success/Skipped/no-job, `1`
/// Failure/Cancelled or a listener error, `2` env/config error before
/// polling, `124` the deadline watchdog fired (takes precedence).
pub(crate) async fn cmd_boot() -> i32 {
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  if let Err(e) = crate::init_tracing_for(&masker) {
    eprintln!("toolu-runner: boot: startup init: {e}");
    return 2;
  }

  let (jit_config_base64, deadline_ms) = match resolve_boot_inputs() {
    Ok(inputs) => inputs,
    Err(code) => return code,
  };

  // The JIT blob carries the runner's RSA private key and OAuth client id:
  // register it with the masker (already wired into the tracing redactor
  // above) before anything can log it, so `boot` never depends on the
  // engine's job-message mask hints to keep its own credential out of
  // `_diag/runner.log`.
  register_jit_secret(&masker, &jit_config_base64);

  let cancel = CancellationToken::new();
  crate::run_cmd::spawn_signal_bridge(cancel.clone());

  let deadline_hit = Arc::new(AtomicBool::new(false));
  let watchdog =
    deadline_ms.map(|deadline| spawn_watchdog(deadline, cancel.clone(), Arc::clone(&deadline_hit)));

  let code = run_listener_once(&jit_config_base64, masker, cancel, &deadline_hit).await;

  // The lifecycle is over, so stand the watchdog down: its grace-period
  // sleep must never hard-exit a process that already finished, and joining
  // the handle surfaces a panic in the task instead of dropping it silently.
  if let Some(handle) = watchdog {
    handle.abort();
    if let Err(e) = handle.await
      && e.is_panic()
    {
      tracing::error!(error = %e, "deadline watchdog task panicked");
    }
  }

  code
}

/// Register the raw `TOOLU_JITCONFIG` blob with the shared masker so the
/// tracing redactor scrubs it from every subsequent line. Lock poisoning is
/// logged, not fatal — a boot that cannot mask still has to run the job, and
/// the blob is not logged on any known path.
fn register_jit_secret(masker: &Arc<Mutex<SecretMasker>>, jit_config_base64: &str) {
  if let Ok(mut m) = masker.lock() {
    m.add_secret(jit_config_base64);
  } else {
    tracing::warn!("secret masker mutex poisoned; TOOLU_JITCONFIG not registered");
  }
}

/// Read + validate env inputs before any network call: `TOOLU_JITCONFIG`
/// (required) and `TOOLU_DEADLINE` (optional, already-past checked here so
/// a stale container never dials out). `Err(code)` is the process exit
/// code for `cmd_boot` to return immediately.
fn resolve_boot_inputs() -> Result<(String, Option<u64>), i32> {
  let Some(jit_config_base64) = read_jit_config() else {
    eprintln!(
      "toolu-runner: boot: TOOLU_JITCONFIG is not set (or empty) — the container provider must \
       pass the encoded JIT config from generate-jitconfig; see `toolu-runner boot --help`"
    );
    return Err(2);
  };

  let deadline_ms = read_deadline();
  if let Some(deadline) = deadline_ms
    && deadline_has_passed(deadline)
  {
    tracing::error!(
      deadline_ms = deadline,
      "TOOLU_DEADLINE already past; exiting before any network call"
    );
    return Err(124);
  }

  Ok((jit_config_base64, deadline_ms))
}

/// Build the listener and run one JIT lifecycle, mapping the outcome (and
/// the watchdog's `deadline_hit` flag, checked last so it always wins) to
/// the process exit code.
async fn run_listener_once(
  jit_config_base64: &str,
  masker: Arc<Mutex<SecretMasker>>,
  cancel: CancellationToken,
  deadline_hit: &Arc<AtomicBool>,
) -> i32 {
  let client = match build_boot_client() {
    Ok(c) => c,
    Err(e) => {
      eprintln!("toolu-runner: boot: {e}");
      return 2;
    },
  };

  let listener = match GitHubListener::new(
    jit_config_base64,
    build_boot_runner_config(),
    masker,
    client,
  ) {
    Ok(l) => l,
    Err(e) => {
      eprintln!("toolu-runner: boot: {e}");
      return 2;
    },
  };

  let result = listener.run(cancel).await;

  // The watchdog's hard exit races this return only after
  // `WATCHDOG_GRACE`; if the deadline flag is set, the job either never
  // finished in time or finished exactly because the watchdog cancelled
  // it — either way 124 takes precedence over whatever `result` says.
  if deadline_hit.load(Ordering::SeqCst) {
    return 124;
  }

  exit_code_for(result)
}

/// Map the listener's outcome to an exit code (deadline handled by the
/// caller, before this is consulted).
fn exit_code_for(result: Result<Option<Conclusion>, RunnerError>) -> i32 {
  match result {
    Ok(None | Some(Conclusion::Success | Conclusion::Skipped)) => 0,
    Ok(Some(Conclusion::Failure | Conclusion::Cancelled)) => 1,
    Err(e) => {
      eprintln!("toolu-runner: boot: {e}");
      1
    },
  }
}

/// Read `TOOLU_JITCONFIG`; `None` for unset or empty.
fn read_jit_config() -> Option<String> {
  match std::env::var("TOOLU_JITCONFIG") {
    Ok(v) if !v.is_empty() => Some(v),
    _ => None,
  }
}

/// Read + parse `TOOLU_DEADLINE`; `None` (with a WARN) for unset or
/// unparseable — the deadline is a backstop, not authority, so `boot` runs
/// without a watchdog rather than refusing to start.
fn read_deadline() -> Option<u64> {
  let Ok(v) = std::env::var("TOOLU_DEADLINE") else {
    tracing::warn!("TOOLU_DEADLINE not set; running without a watchdog");
    return None;
  };
  let parsed = parse_deadline_ms(&v);
  if parsed.is_none() {
    tracing::warn!(
      value = %v,
      "TOOLU_DEADLINE did not parse as a decimal epoch-ms timestamp; running without a watchdog"
    );
  }
  parsed
}

/// Parse a strict decimal epoch-milliseconds string (`/^\d+$/` — no sign,
/// no whitespace, no leading `+`). Pure so exit-code mapping around it is
/// unit-testable without touching the environment.
pub(crate) fn parse_deadline_ms(s: &str) -> Option<u64> {
  if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
    return None;
  }
  s.parse::<u64>().ok()
}

/// Current wall-clock time as epoch milliseconds, saturating instead of
/// panicking on a `SystemTime` before the epoch or a `u128` overflow (both
/// practically unreachable, but `expect`/`unwrap` are denied workspace-wide).
///
/// Both saturations WARN: either one can shift the watchdog's view of the
/// deadline by decades, so a clock that fires `boot` early must leave a
/// diagnostic behind rather than look like a normal timeout.
fn now_epoch_ms() -> u64 {
  let elapsed = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
    Ok(elapsed) => elapsed,
    Err(e) => {
      tracing::warn!(
        error = %e,
        "system clock is before the unix epoch; treating now as epoch 0 — the deadline watchdog \
         may fire late"
      );
      Duration::ZERO
    },
  };
  let Ok(millis) = u64::try_from(elapsed.as_millis()) else {
    tracing::warn!(
      "system clock is past the u64 epoch-ms range; saturating at u64::MAX — the deadline \
       watchdog may fire immediately"
    );
    return u64::MAX;
  };
  millis
}

/// Whether `deadline_ms` is at or before the current time.
fn deadline_has_passed(deadline_ms: u64) -> bool {
  now_epoch_ms() >= deadline_ms
}

/// The in-memory `RunnerConfig` `boot` builds instead of loading
/// `config.toml`: `data_dir` honors `TOOLU_RUNNER_HOME` (the image pins it
/// to `/var/lib/toolu-runner` so the pre-seeded Node cache resolves), every
/// other field mirrors `run_cmd`'s per-iteration defaults for a fresh
/// registration.
fn build_boot_runner_config() -> RunnerConfig {
  let data_dir = config::registry::runner_home();
  RunnerConfig {
    workspace_root: data_dir.join("_work"),
    data_dir,
    cgroup_path: None,
    services_mode: ServicesMode::Forwarder,
    service_bind: RunnerConfig::DEFAULT_SERVICE_BIND.to_owned(),
    cache: CacheConfig::default(),
    workspace_gc_hours: RunnerConfig::DEFAULT_WORKSPACE_GC_HOURS,
    shadow_enabled: false,
  }
}

/// One HTTP client for the single listener lifecycle. Mirrors
/// `run_cmd::build_loop_client`'s 60 s timeout (there is no re-mint loop to
/// share it across, but the listener's own request timeout expectations are
/// the same).
fn build_boot_client() -> Result<reqwest::Client, String> {
  reqwest::Client::builder()
    .timeout(Duration::from_secs(60))
    .build()
    .map_err(|e| format!("HTTP client: {e}"))
}

/// Spawn the deadline watchdog: sleep until `deadline_ms` (standing down
/// early if `cancel` fires first — a plain SIGINT/SIGTERM is not a deadline
/// hit), then fire `cancel` for a graceful shutdown (the job reports
/// `Cancelled` to GitHub) and set `deadline_hit`. If the graceful path
/// hasn't exited the process within [`WATCHDOG_GRACE`], hard-exit 124.
///
/// The returned handle is owned by [`cmd_boot`], which aborts and joins it
/// once the lifecycle ends — so the grace-period sleep cannot outlive the
/// job, and a panic in the task is logged rather than silently swallowed.
fn spawn_watchdog(
  deadline_ms: u64,
  cancel: CancellationToken,
  deadline_hit: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
  tokio::spawn(async move {
    let now = now_epoch_ms();
    let remaining = Duration::from_millis(deadline_ms.saturating_sub(now));
    tokio::select! {
      () = cancel.cancelled() => return,
      () = tokio::time::sleep(remaining) => {},
    }

    deadline_hit.store(true, Ordering::SeqCst);
    tracing::error!("TOOLU_DEADLINE reached; cancelling the in-flight job");
    cancel.cancel();

    tokio::time::sleep(WATCHDOG_GRACE).await;
    tracing::error!("deadline grace period elapsed without a clean exit; hard-exiting 124");
    // `std::process::exit` runs no destructors, so flush the stderr sink by
    // hand before it: the line above must reach the provider's log even on
    // this path. The `_diag` file sink is `tracing_appender::rolling`
    // (synchronous, no background worker), so it is already durable.
    drop(std::io::Write::flush(&mut std::io::stderr()));
    std::process::exit(124);
  })
}
