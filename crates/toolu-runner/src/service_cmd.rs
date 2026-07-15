//! `install-service` subcommand: generate + activate a supervisor unit.
//!
//! Writes a launchd LaunchAgent (macOS) or systemd user unit (Linux) that
//! wraps `run --config <path>` so the runner survives crashes and reboots.
//! The unit text comes from `config::service_unit`; this module owns config
//! resolution, service identity, file destinations, and the platform
//! activation shell-outs (`launchctl` / `systemctl --user`). No network, no
//! tracing init — like `status`, it only touches local files and subprocesses.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use config::config::{load_config as load_reg_config, resolve_data_dir};
use config::service_unit::{ServiceSpec, launchd_plist, systemd_unit};
use shared::RunnerError;

use crate::cli::InstallServiceArgs;

/// The OS supervisor a unit targets. Selected at runtime from the build's
/// target OS; anything but macOS/Linux is rejected before any work.
#[derive(Clone, Copy)]
enum Supervisor {
  /// macOS launchd user LaunchAgent.
  Launchd,
  /// Linux systemd user unit.
  Systemd,
}

/// A registration's supervisor identity: the launchd `Label` (also the
/// systemd `Description` suffix) and the systemd unit file basename.
struct ServiceId {
  /// launchd `Label` + plist basename stem; systemd `Description` suffix.
  label: String,
  /// systemd unit file basename (`toolu-runner-<owner>-<repo>.service`).
  unit: String,
}

/// `install-service`: derive the unit for the resolved registration, then
/// print / write / activate / remove it per the flags. Resolves the config
/// exactly like `run` (`--config` > cwd-inferred repo > sole registration).
pub(crate) fn cmd_install_service(
  args: InstallServiceArgs,
) -> Result<(), Box<dyn std::error::Error>> {
  let supervisor = current_supervisor()?;
  let config_path = crate::resolve_config(args.config)?;
  let id = service_id(&config_path);

  // `--print` writes nothing and needs no HOME, so it returns before the
  // destination is computed (clap forbids `--print` with `--remove`).
  if args.print {
    print!("{}", render_unit(supervisor, &config_path, &id)?);
    return Ok(());
  }

  let dest = dest_path(supervisor, &id, &home_dir()?);
  if args.remove {
    return remove_service(supervisor, &dest, &id);
  }

  let unit = render_unit(supervisor, &config_path, &id)?;
  write_unit(&dest, &unit)?;
  if args.no_activate {
    println!("wrote {}", dest.display());
    println!(
      "activate it with:\n  {}",
      activation_hint(supervisor, &dest, &id)
    );
    return Ok(());
  }
  activate(supervisor, &dest, &id)?;
  println!(
    "installed {} and activated it at {}",
    id.label,
    dest.display()
  );
  Ok(())
}

/// The supervisor for the build's target OS: launchd on macOS, systemd on
/// Linux. Any other target errors — Windows service support is out of scope.
fn current_supervisor() -> Result<Supervisor, RunnerError> {
  if cfg!(target_os = "macos") {
    Ok(Supervisor::Launchd)
  } else if cfg!(target_os = "linux") {
    Ok(Supervisor::Systemd)
  } else {
    Err(RunnerError::Config(
      "install-service supports only launchd (macOS) and systemd (Linux)".to_owned(),
    ))
  }
}

/// Derive the service identity from a registration's config path. A per-repo
/// config (`.../runners/<owner>/<repo>/config.toml`) yields
/// `io.toolu.runner.<owner>.<repo>` / `toolu-runner-<owner>-<repo>.service`;
/// any other layout (the legacy `<home>/config.toml`) falls back to
/// `io.toolu.runner` / `toolu-runner.service`.
fn service_id(config_path: &Path) -> ServiceId {
  match owner_repo(config_path) {
    Some((owner, repo)) => ServiceId {
      label: format!("io.toolu.runner.{owner}.{repo}"),
      unit: format!("toolu-runner-{owner}-{repo}.service"),
    },
    None => ServiceId {
      label: "io.toolu.runner".to_owned(),
      unit: "toolu-runner.service".to_owned(),
    },
  }
}

/// Extract `(owner, repo)` from a `.../runners/<owner>/<repo>/config.toml`
/// path, or `None` when the config is not under a `runners/` tree (legacy
/// root registration).
fn owner_repo(config_path: &Path) -> Option<(String, String)> {
  let repo_dir = config_path.parent()?;
  let owner_dir = repo_dir.parent()?;
  if owner_dir.parent()?.file_name()?.to_str()? != "runners" {
    return None;
  }
  let owner = owner_dir.file_name()?.to_str()?.to_owned();
  let repo = repo_dir.file_name()?.to_str()?.to_owned();
  Some((owner, repo))
}

/// OS-conventional destination for the unit file, honoring `$HOME`.
/// macOS: `~/Library/LaunchAgents/<label>.plist`; Linux:
/// `~/.config/systemd/user/<unit>` (parent dirs created by [`write_unit`]).
fn dest_path(supervisor: Supervisor, id: &ServiceId, home: &Path) -> PathBuf {
  match supervisor {
    Supervisor::Launchd => home
      .join("Library/LaunchAgents")
      .join(format!("{}.plist", id.label)),
    Supervisor::Systemd => home.join(".config/systemd/user").join(&id.unit),
  }
}

/// The user's home from `$HOME`; unit files land under it.
fn home_dir() -> Result<PathBuf, RunnerError> {
  std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
    RunnerError::Config("HOME is not set — cannot locate the user service directory".to_owned())
  })
}

/// Load `config_path`, derive its `_diag` dir, and render the platform unit
/// text. `exe` is the absolute running binary; `config_path` is canonicalized
/// so the supervisor's `ExecStart` / `ProgramArguments` are host-absolute.
fn render_unit(
  supervisor: Supervisor,
  config_path: &Path,
  id: &ServiceId,
) -> Result<String, Box<dyn std::error::Error>> {
  let cfg = load_reg_config(config_path)?;
  let diag_dir = resolve_data_dir(&cfg.runtime.data_dir)?.join("_diag");
  let exe = std::env::current_exe()?;
  let abs_config = config_path.canonicalize()?;
  let spec = ServiceSpec {
    label: &id.label,
    exe: &exe,
    config_path: &abs_config,
    diag_dir: &diag_dir,
  };
  Ok(match supervisor {
    Supervisor::Launchd => launchd_plist(&spec),
    Supervisor::Systemd => systemd_unit(&spec),
  })
}

/// Write the unit text to `dest`, creating parent dirs (0644 content).
fn write_unit(dest: &Path, unit: &str) -> Result<(), std::io::Error> {
  if let Some(parent) = dest.parent() {
    std::fs::create_dir_all(parent)?;
  }
  std::fs::write(dest, unit)
}

/// `--remove`: deactivate the unit (best-effort) and delete the file.
/// Idempotent — a missing file reports nothing-to-do and exits 0. Deletion
/// failure IS an error; deactivation failure (unit never loaded) is not.
fn remove_service(
  supervisor: Supervisor,
  dest: &Path,
  id: &ServiceId,
) -> Result<(), Box<dyn std::error::Error>> {
  if !dest.exists() {
    println!("no service installed at {} — nothing to do", dest.display());
    return Ok(());
  }
  deactivate(supervisor, dest, id);
  // Tolerate a concurrent delete between the exists() check and here —
  // idempotency must hold under the race, not just sequentially.
  match std::fs::remove_file(dest) {
    Ok(()) => {
      println!("removed {} ({})", id.label, dest.display());
      Ok(())
    },
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
      println!("no service installed at {} — nothing to do", dest.display());
      Ok(())
    },
    Err(e) => Err(e.into()),
  }
}

/// Activate the just-written unit. A non-zero manager exit is an error
/// naming the command and its stderr (never swallowed).
fn activate(supervisor: Supervisor, dest: &Path, id: &ServiceId) -> Result<(), RunnerError> {
  match supervisor {
    Supervisor::Launchd => activate_launchd(dest),
    Supervisor::Systemd => activate_systemd(&id.unit),
  }
}

/// Load a plist into the per-user launchd domain. `bootstrap gui/<uid>` is
/// the modern (macOS 10.13+) loader; a non-zero exit (older host) falls back
/// to the legacy `launchctl load -w`, whose failure surfaces as the error.
fn activate_launchd(plist: &Path) -> Result<(), RunnerError> {
  let domain = format!("gui/{}", current_uid()?);
  let plist = plist.to_string_lossy().into_owned();
  if succeeds("launchctl", &["bootstrap", &domain, &plist]) {
    return Ok(());
  }
  run_checked("launchctl", &["load", "-w", &plist])
}

/// Reload the user manager and enable + start the unit.
fn activate_systemd(unit: &str) -> Result<(), RunnerError> {
  run_checked("systemctl", &["--user", "daemon-reload"])?;
  run_checked("systemctl", &["--user", "enable", "--now", unit])
}

/// Best-effort deactivation for `--remove`: a failure here (unit never
/// loaded, unknown to the manager) is reported, not fatal — deleting the
/// file is what removal guarantees.
fn deactivate(supervisor: Supervisor, dest: &Path, id: &ServiceId) {
  let result = match supervisor {
    Supervisor::Launchd => deactivate_launchd(dest, &id.label),
    Supervisor::Systemd => deactivate_systemd(&id.unit),
  };
  if let Err(e) = result {
    eprintln!("warning: deactivation failed (unit may not have been loaded): {e}");
  }
}

/// Unload a plist from the per-user launchd domain: modern
/// `bootout gui/<uid>/<label>`, falling back to legacy `launchctl unload`.
fn deactivate_launchd(plist: &Path, label: &str) -> Result<(), RunnerError> {
  let target = format!("gui/{}/{label}", current_uid()?);
  if succeeds("launchctl", &["bootout", &target]) {
    return Ok(());
  }
  run_checked("launchctl", &["unload", &plist.to_string_lossy()])
}

/// Disable + stop the unit, then reload the user manager.
fn deactivate_systemd(unit: &str) -> Result<(), RunnerError> {
  run_checked("systemctl", &["--user", "disable", "--now", unit])?;
  run_checked("systemctl", &["--user", "daemon-reload"])
}

/// The exact command `--no-activate` tells the user to run — it mirrors what
/// the default mode would execute to load the just-written unit.
fn activation_hint(supervisor: Supervisor, dest: &Path, id: &ServiceId) -> String {
  match supervisor {
    Supervisor::Launchd => format!("launchctl bootstrap gui/$(id -u) {}", dest.display()),
    Supervisor::Systemd => format!(
      "systemctl --user daemon-reload && systemctl --user enable --now {}",
      id.unit
    ),
  }
}

/// The current user id via `id -u` (avoids an `unsafe` libc `getuid`).
fn current_uid() -> Result<String, RunnerError> {
  run_stdout("id", &["-u"])
}

/// Run `program args`, capturing output. An IO failure (e.g. the binary is
/// missing) is an error naming the command line. Runner secrets are dropped
/// from the child env — `launchctl`/`systemctl` don't need them (full
/// `env_clear` would break `systemctl --user`, which needs XDG/DBus vars).
fn run_capture(program: &str, args: &[&str]) -> Result<Output, RunnerError> {
  Command::new(program)
    .args(args)
    .env_remove("TOOLU_RUNNER_TOKEN")
    .env_remove("TOOLU_RUNNER_CLIENT_ID")
    .output()
    .map_err(|e| RunnerError::Config(format!("failed to run `{program} {}`: {e}", args.join(" "))))
}

/// Run `program args`, requiring a zero exit; otherwise an error carrying the
/// command line, exit status, and captured stderr.
fn run_checked(program: &str, args: &[&str]) -> Result<(), RunnerError> {
  let output = run_capture(program, args)?;
  if output.status.success() {
    return Ok(());
  }
  Err(command_failed(program, args, &output))
}

/// Run `program args`, requiring a zero exit, and return trimmed stdout.
fn run_stdout(program: &str, args: &[&str]) -> Result<String, RunnerError> {
  let output = run_capture(program, args)?;
  if !output.status.success() {
    return Err(command_failed(program, args, &output));
  }
  Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// `true` if `program args` ran and exited zero; an IO failure or non-zero
/// exit is `false` (used for the modern-then-legacy launchctl fallback).
fn succeeds(program: &str, args: &[&str]) -> bool {
  run_capture(program, args).is_ok_and(|o| o.status.success())
}

/// Build the "command failed" error naming the command, status, and stderr.
fn command_failed(program: &str, args: &[&str], output: &Output) -> RunnerError {
  RunnerError::Config(format!(
    "`{program} {}` failed ({}): {}",
    args.join(" "),
    output.status,
    String::from_utf8_lossy(&output.stderr).trim()
  ))
}
