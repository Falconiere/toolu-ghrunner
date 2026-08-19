//! Binary entrypoint for the toolu compute daemon: the startup sequence that
//! turns a process into a serving `vps_hosts` host.
//!
//! The order is load-bearing, not stylistic:
//!
//! 1. **Configuration**, then **connect to Docker**. Both fail loudly here
//!    rather than turning every job into a 503 later.
//! 2. **Adopt what is already running** (`daemon::adopt`). State lives in
//!    Docker, not in this process, and a restart is routine — rotating the
//!    bearer token restarts the daemon. Seeding the gate from container
//!    labels is what stops a restart from believing the box is empty and
//!    overcommitting it by every container still running.
//! 3. **Pre-pull the pinned image** (`daemon::prepull`), retrying until it is
//!    resident.
//! 4. **Only then bind.** A create against an absent image answers 503, which
//!    `vpsDispositionFor` reads as fallback *plus a five-minute cooldown on
//!    this host*. Binding while the first pull is still running would cool
//!    the only host in the fleet down for five minutes on every restart.
//! 5. **Timers**: the reconcile tick (`DockerBackend::tick`) and the periodic
//!    image refresh, both spawned once the listener is up.
//! 6. **Shutdown** on SIGTERM or Ctrl-C, which is how systemd stops this.

use std::fmt;
use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::PoisonError;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::net::TcpListener;
use tokio::signal::unix::SignalKind;

use daemon::adopt;
use daemon::config::{Config, ConfigError};
use daemon::docker::DockerBackend;
use daemon::gate::{Gate, JobSize};
use daemon::logging::log_filter;
use daemon::prepull::{RefreshDecision, StartupDecision, refresh_decision, startup_decision};
use daemon::routes::state::AppState;

/// How often the reaper-and-scheduler tick runs: release the budget of
/// containers that exited, kill anything past its deadline, start whatever
/// the freed budget promotes. Short enough that a finished job's budget
/// reaches the next job promptly, long enough that the container listing
/// behind it is not a constant load on the Docker daemon.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);

/// How often the pinned image is re-pulled after startup, so a moving tag
/// (`vps_hosts.image_ref` may point at one) is picked up without a restart.
const IMAGE_REFRESH_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// How many times startup pulls before giving up and failing the process.
/// Exiting hands the retry to the supervisor with a clean slate, which is
/// better than binding a listener that can only answer 503.
const PREPULL_ATTEMPTS: u32 = 5;

/// How long startup waits between pull attempts.
const PREPULL_RETRY_DELAY: Duration = Duration::from_secs(5);

/// Why the daemon could not start, or could not keep serving.
#[derive(Debug)]
enum StartupError {
  /// Configuration was missing or unusable.
  Config(ConfigError),
  /// The Docker daemon could not be reached.
  Connect(bollard::errors::Error),
  /// Existing job containers could not be listed, so the gate cannot be
  /// seeded — see `DockerBackend::existing_jobs`.
  Inventory(bollard::errors::Error),
  /// The pinned image never became resident.
  ImageUnavailable {
    /// The image that could not be pulled.
    image: String,
    /// How many attempts were made.
    attempts: u32,
  },
  /// The listener could not be bound.
  Bind {
    /// The address that was refused.
    addr: SocketAddr,
    /// The underlying I/O error.
    source: std::io::Error,
  },
  /// The HTTP server stopped with an error.
  Serve(std::io::Error),
  /// The system clock is before the Unix epoch, so no deadline decision can
  /// be made.
  ClockBeforeEpoch(std::time::SystemTimeError),
  /// The system clock is further from the epoch than epoch milliseconds can
  /// express.
  ClockOutOfRange(std::num::TryFromIntError),
}

impl fmt::Display for StartupError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Config(source) => write!(f, "configuration is unusable: {source}"),
      Self::Connect(source) => write!(f, "cannot reach the Docker daemon: {source}"),
      Self::Inventory(source) => {
        write!(f, "cannot list existing job containers to adopt: {source}")
      },
      Self::ImageUnavailable { image, attempts } => write!(
        f,
        "the pinned image {image} is still not resident after {attempts} pull attempts"
      ),
      Self::Bind { addr, source } => write!(f, "cannot bind {addr}: {source}"),
      Self::Serve(source) => write!(f, "the HTTP server stopped: {source}"),
      Self::ClockBeforeEpoch(source) => write!(f, "the system clock is before 1970: {source}"),
      Self::ClockOutOfRange(source) => {
        write!(
          f,
          "the system clock is out of epoch-millisecond range: {source}"
        )
      },
    }
  }
}

impl std::error::Error for StartupError {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    match self {
      Self::Config(source) => Some(source),
      Self::Connect(source) | Self::Inventory(source) => Some(source),
      Self::Bind { source, .. } | Self::Serve(source) => Some(source),
      Self::ClockBeforeEpoch(source) => Some(source),
      Self::ClockOutOfRange(source) => Some(source),
      Self::ImageUnavailable { .. } => None,
    }
  }
}

/// Wall-clock epoch milliseconds — the clock every deadline decision is made
/// against, read here and passed in, so nothing downstream reads one of its
/// own.
///
/// # Errors
///
/// Returns [`StartupError::ClockBeforeEpoch`] or
/// [`StartupError::ClockOutOfRange`] when the system clock cannot be
/// expressed as epoch milliseconds at all.
fn epoch_ms() -> Result<i64, StartupError> {
  let since_epoch = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_err(StartupError::ClockBeforeEpoch)?;
  i64::try_from(since_epoch.as_millis()).map_err(StartupError::ClockOutOfRange)
}

/// The whole box's budget, as the gate accounts it.
fn budget_of(config: &Config) -> JobSize {
  JobSize {
    vcpu: config.vcpu,
    memory_mb: config.memory_mb,
  }
}

fn main() -> ExitCode {
  // `RUST_LOG` decides everything except bollard's ceiling — see
  // `daemon::logging`, which is where the reason that ceiling exists lives.
  tracing_subscriber::fmt()
    .with_env_filter(log_filter(daemon::config::log_directives().as_deref()))
    .init();

  let runtime = match tokio::runtime::Runtime::new() {
    Ok(runtime) => runtime,
    Err(err) => {
      tracing::error!(error = %err, "toolu-daemon could not start its async runtime");
      return ExitCode::FAILURE;
    },
  };

  match runtime.block_on(run()) {
    Ok(()) => ExitCode::SUCCESS,
    Err(err) => {
      tracing::error!(error = %err, "toolu-daemon stopped");
      ExitCode::FAILURE
    },
  }
}

/// The startup sequence, in the order the module docs pin.
///
/// # Errors
///
/// Returns [`StartupError`] for every startup fault: unusable configuration,
/// an unreachable Docker daemon, an inventory that cannot be read, an image
/// that never becomes resident, an address that cannot be bound, or a server
/// that stops with an error.
async fn run() -> Result<(), StartupError> {
  let config = Config::from_env().map_err(StartupError::Config)?;
  let backend = DockerBackend::connect(&config.runtime).map_err(StartupError::Connect)?;
  let state = AppState::new(
    backend.clone(),
    Gate::new(budget_of(&config), config.queue_max),
    config.token_file.clone(),
    &config.image,
  );

  adopt_existing_jobs(&backend, &state).await?;
  pull_until_resident(&backend, &config.image).await?;

  let listener = TcpListener::bind(config.bind)
    .await
    .map_err(|source| StartupError::Bind {
      addr: config.bind,
      source,
    })?;
  tracing::info!(
    bind = %config.bind,
    image = config.image.as_str(),
    vcpu = config.vcpu,
    memory_mb = config.memory_mb,
    "toolu-daemon is serving"
  );

  serve(listener, state, backend, config).await
}

/// Rebuild the gate, the start queue and the created-container map from the
/// job containers Docker still holds, then remove whatever adoption decided
/// is finished, expired or unrunnable. Runs before the listener exists, so no
/// request can ever be admitted against an empty budget.
async fn adopt_existing_jobs(
  backend: &DockerBackend,
  state: &AppState<DockerBackend>,
) -> Result<(), StartupError> {
  let containers = backend
    .existing_jobs()
    .await
    .map_err(StartupError::Inventory)?;
  let now_ms = epoch_ms()?;

  // The three guards below are `std::sync::Mutex` guards, held together for
  // one synchronous stretch. NOTHING inside this block may `.await`: a
  // std guard held across an await point is held across a task yield, and
  // the next task to reach the same lock on this thread deadlocks. That is
  // why `adopt::adopt` is synchronous and why `backend.remove_all` — the
  // one await this function needs — is outside the scope, below.
  let (adoption, consumption) = {
    let mut gate = state.gate.lock().unwrap_or_else(PoisonError::into_inner);
    let mut queue = state
      .start_queue
      .lock()
      .unwrap_or_else(PoisonError::into_inner);
    let mut created = state
      .created_containers
      .lock()
      .unwrap_or_else(PoisonError::into_inner);
    let adoption = adopt::adopt(&mut gate, &mut queue, &mut created, &containers, now_ms);
    let consumption = gate.consumption();
    (adoption, consumption)
  };

  tracing::info!(
    found = containers.len(),
    resumed = adoption.resumed.len(),
    requeued = adoption.requeued.len(),
    removing = adoption.remove.len(),
    vcpu_used = consumption.vcpu_used,
    memory_mb_used = consumption.memory_mb_used,
    "adopted the job containers this box was already running"
  );

  backend.remove_all(&adoption.remove).await;
  Ok(())
}

/// Pull the pinned image until it is resident, or fail startup trying. The
/// listener is not bound until this returns `Ok` — see the module docs.
///
/// # Errors
///
/// Returns [`StartupError::ImageUnavailable`] once the attempts are spent
/// and the image is still absent.
async fn pull_until_resident(backend: &DockerBackend, image: &str) -> Result<(), StartupError> {
  for attempt in 1..=PREPULL_ATTEMPTS {
    let outcome = backend.attempt_pull(image).await;
    match startup_decision(outcome, PREPULL_ATTEMPTS - attempt) {
      StartupDecision::Bind => {
        tracing::info!(image, attempt, "the pinned image is resident");
        return Ok(());
      },
      StartupDecision::Retry => {
        tracing::warn!(
          image,
          attempt,
          "the pinned image is not resident yet; retrying before binding"
        );
        tokio::time::sleep(PREPULL_RETRY_DELAY).await;
      },
      StartupDecision::GiveUp => break,
    }
  }

  Err(StartupError::ImageUnavailable {
    image: image.to_owned(),
    attempts: PREPULL_ATTEMPTS,
  })
}

/// Serve the router on `listener` until a shutdown signal arrives, with the
/// reconcile tick and the image refresh running alongside it.
///
/// # Errors
///
/// Returns [`StartupError::Serve`] if the HTTP server itself fails.
async fn serve(
  listener: TcpListener,
  state: AppState<DockerBackend>,
  backend: DockerBackend,
  config: Config,
) -> Result<(), StartupError> {
  let reconciler = tokio::spawn(reconcile_loop(backend.clone(), state.clone()));
  let refresher = tokio::spawn(refresh_loop(backend, config.image));
  let router = daemon::routes::build_router(state);

  let served = axum::serve(listener, router)
    .with_graceful_shutdown(shutdown_signal())
    .await;

  // No JOB state to drain: both loops read theirs fresh from Docker on every
  // tick, so stopping between ticks loses nothing a job depends on.
  //
  // One thing is genuinely dropped: an image pull in flight in `refresh_loop`
  // is cancelled mid-stream, and the layers it had already fetched stay in
  // Docker's storage as a partial pull. That costs nothing — the next
  // `attempt_pull`, at the next start or the next refresh tick, resumes from
  // those layers — and waiting the pull out would hold shutdown for however
  // long a registry takes.
  reconciler.abort();
  refresher.abort();

  served.map_err(StartupError::Serve)
}

/// Run `DockerBackend::tick` on a timer for as long as the daemon serves. A
/// clock that cannot be read skips the tick rather than guessing: every
/// decision in a tick is a deadline comparison, and a wrong `now` would kill
/// live jobs.
async fn reconcile_loop(backend: DockerBackend, state: AppState<DockerBackend>) {
  let mut ticker = tokio::time::interval(RECONCILE_INTERVAL);
  loop {
    ticker.tick().await;
    match epoch_ms() {
      Ok(now_ms) => {
        backend
          .tick(
            &state.gate,
            &state.start_queue,
            &state.created_containers,
            now_ms,
          )
          .await;
      },
      Err(err) => {
        tracing::warn!(error = %err, "skipping a reconcile tick: the clock is unreadable");
      },
    }
  }
}

/// Re-pull the pinned image on a timer. Never stops serving, whatever it
/// finds — see `daemon::prepull::RefreshDecision`.
async fn refresh_loop(backend: DockerBackend, image: String) {
  let mut ticker = tokio::time::interval(IMAGE_REFRESH_INTERVAL);
  // A tokio interval's first tick completes immediately, and the startup
  // pull already covered it.
  ticker.tick().await;

  loop {
    ticker.tick().await;
    match refresh_decision(backend.attempt_pull(&image).await) {
      RefreshDecision::KeepServing => {
        tracing::info!(image = image.as_str(), "refreshed the pinned image");
      },
      RefreshDecision::WarnMissingImage => {
        tracing::warn!(
          image = image.as_str(),
          "the pinned image is not resident; creates answer 503 until a refresh succeeds"
        );
      },
    }
  }
}

/// Resolve on SIGTERM (how systemd stops a unit) or Ctrl-C (how an operator
/// stops one by hand). A signal listener that cannot be installed waits
/// forever rather than resolving: reporting a shutdown nobody asked for would
/// drop every job the box is running.
///
/// The consequence, stated rather than left to be discovered: if BOTH
/// installs fail, this future never resolves and the daemon stops responding
/// to a graceful stop entirely. systemd then waits out `TimeoutStopSec=30`
/// (`scripts/toolu-daemon.service`) and SIGKILLs it — the jobs die either
/// way, 30 seconds later. That is the deliberate
/// trade. Silence is not a failure mode here: each failed install is logged
/// at ERROR the moment it happens, so "the unit would not stop" always has
/// its cause in the journal directly above it. Installing a signal handler
/// only fails on a process that is out of file descriptors or running under
/// a sandbox that blocks `signalfd`, neither of which is a state this daemon
/// can serve jobs from anyway.
async fn shutdown_signal() {
  let interrupt = async {
    match tokio::signal::ctrl_c().await {
      Ok(()) => tracing::info!("received ctrl-c; shutting down"),
      Err(err) => {
        tracing::error!(error = %err, "cannot listen for ctrl-c");
        std::future::pending::<()>().await;
      },
    }
  };

  let terminate = async {
    match tokio::signal::unix::signal(SignalKind::terminate()) {
      Ok(mut signals) => {
        signals.recv().await;
        tracing::info!("received SIGTERM; shutting down");
      },
      Err(err) => {
        tracing::error!(error = %err, "cannot listen for SIGTERM");
        std::future::pending::<()>().await;
      },
    }
  };

  tokio::select! {
    () = interrupt => {},
    () = terminate => {},
  }
}
