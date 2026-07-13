//! `toolu-runner watch` — ratatui TUI over the job journal
//! (`<data_dir>/_diag/jobs/`): job history list, live step tree + log
//! tail for the selected job, and a SIGINT cancel key (unix only).
//! Without a usable config it browses every registered
//! `runners/<owner>/<repo>/` jobs dir plus the legacy home, merged —
//! multi-dir browsing is what backs the per-repo runner layout.

/// Keyboard mapping: key events → `Action`s (incl. the confirm modal).
pub mod input;
/// Pure reducer: journal lines → job list / step tree / log ring.
pub mod state;
/// Ratatui rendering, pure view over `state::App`.
pub mod ui;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyEventKind};
use shared::RunnerError;

use crate::journal::reader::{JobSummary, JournalReader, scan_jobs};
use crate::journal::writer::jobs_dir_for;
use input::Action;
use state::{App, OpenJob, Pane};

/// Input poll tick — also the journal tail cadence.
const TICK: Duration = Duration::from_millis(250);
/// Jobs-dir rescan cadence.
const RESCAN: Duration = Duration::from_secs(1);

/// Everything the event loop needs besides the pure `App` state.
struct WatchCtx {
  /// Jobs dirs the rescan merges; one entry for a registered config,
  /// one per discovered registration (+ legacy) in the fallback.
  jobs_dirs: Vec<PathBuf>,
  lock_path: PathBuf,
  /// `Some(home)` = unregistered fallback: `jobs_dirs` is re-discovered
  /// from the registry on every rescan so new registrations appear live.
  discover_home: Option<PathBuf>,
  reader: Option<JournalReader>,
  last_scan: Option<Instant>,
}

/// Run the watch TUI until the user quits.
///
/// Config resolution is forgiving: a missing/unreadable config falls back
/// to browsing every registered runner dir (plus the legacy home) so pure
/// history browsing still works.
///
/// # Errors
///
/// Returns `RunnerError::Config` when the terminal cannot be initialized
/// or restored — never for journal/config problems.
pub fn run_watch(config_path: &Path) -> Result<(), RunnerError> {
  let (runner_name, mut ctx) = context_for(config_path);
  let mut app = App::new(runner_name);

  let mut terminal =
    ratatui::try_init().map_err(|e| RunnerError::Config(format!("terminal init failed: {e}")))?;
  let result = event_loop(&mut terminal, &mut app, &mut ctx);
  if let Err(e) = ratatui::try_restore() {
    tracing::warn!(error = %e, "terminal restore failed");
  }
  result.map_err(|e| RunnerError::Config(format!("watch terminal error: {e}")))
}

/// Runner display name + watch context, with the unregistered fallback.
/// A readable config keeps the single-dir behavior over its `data_dir`;
/// the fallback browses all registered runner dirs merged (per-repo layout).
fn context_for(config_path: &Path) -> (String, WatchCtx) {
  match config::config::load_config(config_path) {
    Ok(cfg) => {
      let data_dir = config::config::resolve_data_dir(&cfg.runtime.data_dir)
        .unwrap_or_else(|_| config::registry::runner_home());
      let ctx = WatchCtx {
        jobs_dirs: vec![jobs_dir_for(&data_dir)],
        lock_path: data_dir.join(".lock"),
        discover_home: None,
        reader: None,
        last_scan: None,
      };
      (cfg.runner_name, ctx)
    },
    Err(e) => {
      tracing::warn!(error = %e, "config unreadable; browsing all registered runner dirs");
      let home = config::registry::runner_home();
      let ctx = WatchCtx {
        jobs_dirs: discover_jobs_dirs(&home),
        lock_path: home.join(".lock"),
        discover_home: Some(home),
        reader: None,
        last_scan: None,
      };
      ("<unregistered>".to_owned(), ctx)
    },
  }
}

/// Jobs dirs to browse under `home`: one per registration found by
/// `config::registry::list_registrations` (the registration dir is the
/// config's parent) plus the legacy `<home>/_diag/jobs`, deduplicated.
/// Pure discovery — no TUI, no reads beyond the registry scan. Mirrors
/// `scan_all_jobs`'s skip-and-continue tolerance: an unreadable registry
/// scan yields no per-repo dirs (the legacy home still browses).
pub fn discover_jobs_dirs(home: &Path) -> Vec<PathBuf> {
  let mut dirs: Vec<PathBuf> = Vec::new();
  let registrations = config::registry::list_registrations(home).unwrap_or_else(|e| {
    tracing::debug!(home = %home.display(), error = %e, "watch: skipping unreadable registrations scan");
    Vec::new()
  });
  for entry in registrations {
    // A rootless config path has no parent dir to hold `_diag/` — skip.
    let Some(reg_dir) = entry.config_path.parent() else {
      continue;
    };
    let jobs = jobs_dir_for(reg_dir);
    if !dirs.contains(&jobs) {
      dirs.push(jobs);
    }
  }
  // Legacy home journals can exist without a legacy config.toml.
  let legacy = jobs_dir_for(home);
  if !dirs.contains(&legacy) {
    dirs.push(legacy);
  }
  dirs
}

/// Merge `scan_jobs` across several jobs dirs, newest first by journal
/// file name (the `<UTC ts>-<job_id>` prefix orders by start time; ties
/// break on the full path). Job identity stays the full journal path
/// (`JobSummary::path`), so same-named files in different dirs never
/// collide. Mirrors the journal's never-fail tolerance: an unreadable or
/// missing dir is skipped (missing just means no jobs ran there yet).
pub fn scan_all_jobs(jobs_dirs: &[PathBuf]) -> Vec<JobSummary> {
  let mut jobs = Vec::new();
  for dir in jobs_dirs {
    match scan_jobs(dir) {
      Ok(mut found) => jobs.append(&mut found),
      Err(e) => {
        tracing::debug!(dir = %dir.display(), error = %e, "watch: skipping unreadable jobs dir");
      },
    }
  }
  jobs.sort_by(|a, b| {
    b.path
      .file_name()
      .cmp(&a.path.file_name())
      .then_with(|| b.path.cmp(&a.path))
  });
  jobs
}

/// Draw / input / tail cycle.
fn event_loop(
  terminal: &mut ratatui::DefaultTerminal,
  app: &mut App,
  ctx: &mut WatchCtx,
) -> std::io::Result<()> {
  loop {
    rescan_if_due(app, ctx);
    tail_opened(app, ctx);
    terminal.draw(|f| ui::render(f, app))?;
    if !crossterm::event::poll(TICK)? {
      continue;
    }
    if let Event::Key(key) = crossterm::event::read()?
      && key.kind == KeyEventKind::Press
    {
      let action = input::action_for(app, key);
      if handle_action(app, ctx, action) {
        return Ok(());
      }
    }
  }
}

/// Refresh the job list + lock header line at the rescan cadence.
fn rescan_if_due(app: &mut App, ctx: &mut WatchCtx) {
  if ctx.last_scan.is_some_and(|t| t.elapsed() < RESCAN) {
    return;
  }
  ctx.last_scan = Some(Instant::now());
  // Unregistered browsing: re-discover so new registrations appear live.
  if let Some(home) = &ctx.discover_home {
    ctx.jobs_dirs = discover_jobs_dirs(home);
  }
  app.set_jobs(scan_all_jobs(&ctx.jobs_dirs));
  app.lock_line = lock_line(&ctx.lock_path);
  if ctx.reader.is_none() && !app.jobs.is_empty() {
    open_job(app, ctx, 0);
  }
}

/// Feed newly appended journal lines into the opened job.
fn tail_opened(app: &mut App, ctx: &mut WatchCtx) {
  let Some(reader) = ctx.reader.as_mut() else {
    return;
  };
  let Some(opened) = app.opened.as_mut() else {
    return;
  };
  match reader.poll() {
    Ok(lines) => opened.apply_all(lines),
    Err(e) => {
      // Journal pruned or unreadable mid-watch: surface, keep the model.
      app.flash = Some(format!("journal read failed: {e}"));
    },
  }
  if app.follow {
    app.scroll_from_bottom = 0;
  }
}

/// Apply one `Action`; `true` means quit.
fn handle_action(app: &mut App, ctx: &mut WatchCtx, action: Action) -> bool {
  app.flash = None;
  match action {
    Action::Quit => return true,
    Action::MoveUp => move_focus_up(app),
    Action::MoveDown => move_focus_down(app),
    Action::OpenSelected => open_job(app, ctx, app.selected),
    Action::TogglePane => {
      app.pane = if app.pane == Pane::Jobs {
        Pane::Detail
      } else {
        Pane::Jobs
      };
    },
    Action::ToggleFollow => app.follow = !app.follow,
    Action::PageUp => {
      app.follow = false;
      app.scroll_from_bottom = app.scroll_from_bottom.saturating_add(10);
    },
    Action::PageDown => app.scroll_from_bottom = app.scroll_from_bottom.saturating_sub(10),
    Action::RequestCancel => app.confirm_cancel = true,
    Action::ConfirmCancel => {
      app.confirm_cancel = false;
      app.flash = Some(match send_cancel(&ctx.lock_path) {
        Ok(pid) => format!("SIGINT sent to runner pid {pid}"),
        Err(e) => format!("cancel failed: {e}"),
      });
    },
    Action::DismissCancel => app.confirm_cancel = false,
    Action::None => {},
  }
  false
}

/// Up routes to the focused pane: job cursor or log scroll (away from tail).
fn move_focus_up(app: &mut App) {
  match app.pane {
    Pane::Jobs => app.select_up(),
    Pane::Detail => {
      app.follow = false;
      app.scroll_from_bottom = app.scroll_from_bottom.saturating_add(1);
    },
  }
}

/// Down routes to the focused pane: job cursor or log scroll (toward tail).
fn move_focus_down(app: &mut App) {
  match app.pane {
    Pane::Jobs => app.select_down(),
    Pane::Detail => app.scroll_from_bottom = app.scroll_from_bottom.saturating_sub(1),
  }
}

/// Open the job list entry at `idx` in the detail pane.
fn open_job(app: &mut App, ctx: &mut WatchCtx, idx: usize) {
  let Some(summary) = app.jobs.get(idx) else {
    return;
  };
  ctx.reader = Some(JournalReader::new(summary.path.clone()));
  app.opened = Some(OpenJob::new(summary.path.clone()));
  app.selected = idx;
  app.scroll_from_bottom = 0;
  app.follow = true;
}

/// Max bytes read from the `.lock` file. Its body is a tiny JSON object;
/// the cap bounds memory if the path is replaced with a huge file.
const LOCK_READ_CAP: u64 = 64 * 1024;

/// Read at most `LOCK_READ_CAP` bytes of the lock file as a string.
fn read_lock_capped(lock_path: &Path) -> std::io::Result<String> {
  use std::io::Read;
  let file = std::fs::File::open(lock_path)?;
  let mut body = String::new();
  file.take(LOCK_READ_CAP).read_to_string(&mut body)?;
  Ok(body)
}

/// Header line describing the `.lock` holder.
fn lock_line(lock_path: &Path) -> String {
  match read_lock_capped(lock_path) {
    Ok(body) => match serde_json::from_str::<config::lockfile::LockBody>(&body) {
      Ok(lock) if config::lockfile::is_pid_alive(lock.pid) => {
        format!("running (pid {}, since {})", lock.pid, lock.started_at)
      },
      Ok(lock) => format!("stale lock (pid {} dead)", lock.pid),
      Err(_) => "lock unreadable".to_owned(),
    },
    Err(_) => "idle".to_owned(),
  }
}

/// Deliver SIGINT to the `.lock` holder (the runner's graceful-cancel path).
/// The target's executable name must contain `toolu-runner`, so a tampered
/// lock file cannot make the TUI signal an unrelated PID. Unix only.
///
/// # Errors
///
/// Fails when the lock file is absent/unreadable, the PID is not running or
/// not a toolu-runner process, or signal delivery is refused.
#[cfg(unix)]
pub fn send_cancel(lock_path: &Path) -> Result<u32, String> {
  let body = read_lock_capped(lock_path).map_err(|e| format!("no lock file: {e}"))?;
  let lock: config::lockfile::LockBody =
    serde_json::from_str(&body).map_err(|e| format!("lock body unreadable: {e}"))?;
  // Guard against signalling an unrelated PID from a tampered or stale
  // (PID-recycled) lock file: the holder must actually be a toolu-runner.
  let mut sys = sysinfo::System::new();
  sys.refresh_processes(
    sysinfo::ProcessesToUpdate::Some(&[sysinfo::Pid::from_u32(lock.pid)]),
    true,
  );
  let process = sys
    .process(sysinfo::Pid::from_u32(lock.pid))
    .ok_or_else(|| format!("runner pid {} not running", lock.pid))?;
  if !is_toolu_runner_process(process) {
    return Err(format!(
      "pid {} is not a toolu-runner process; refusing to signal it",
      lock.pid
    ));
  }
  deliver_sigint(lock.pid).map(|()| lock.pid)
}

/// Whether `process` names toolu-runner in its name, exe path, or argv[0].
#[cfg(unix)]
fn is_toolu_runner_process(process: &sysinfo::Process) -> bool {
  const NEEDLE: &str = "toolu-runner";
  process.name().to_string_lossy().contains(NEEDLE)
    || process
      .exe()
      .is_some_and(|p| p.to_string_lossy().contains(NEEDLE))
    || process
      .cmd()
      .first()
      .is_some_and(|a| a.to_string_lossy().contains(NEEDLE))
}

/// Deliver SIGINT to `pid` via `sysinfo` (the signal mechanics, without the
/// identity gate `send_cancel` applies first).
///
/// # Errors
///
/// Fails when the PID is not running or the signal is refused/unsupported.
#[cfg(unix)]
pub fn deliver_sigint(pid: u32) -> Result<(), String> {
  let mut sys = sysinfo::System::new();
  sys.refresh_processes(
    sysinfo::ProcessesToUpdate::Some(&[sysinfo::Pid::from_u32(pid)]),
    true,
  );
  let process = sys
    .process(sysinfo::Pid::from_u32(pid))
    .ok_or_else(|| format!("pid {pid} not running"))?;
  match process.kill_with(sysinfo::Signal::Interrupt) {
    Some(true) => Ok(()),
    Some(false) => Err(format!("SIGINT to pid {pid} refused")),
    None => Err("SIGINT unsupported on this platform".to_owned()),
  }
}

/// Cancel is unix-only; the key stays inert elsewhere.
#[cfg(not(unix))]
pub fn send_cancel(_lock_path: &Path) -> Result<u32, String> {
  Err("cancel is only supported on unix".to_owned())
}
