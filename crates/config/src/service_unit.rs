//! Supervisor unit rendering for `install-service`.
//!
//! Pure string builders that turn a [`ServiceSpec`] (one registration,
//! `run --config <path>`) or a [`MachineSpec`] (the machine unit,
//! `run --all`, #208) into a launchd user `LaunchAgent` plist (macOS) or a
//! systemd user unit (Linux), plus the inverse — [`parse_systemd_target`] /
//! [`parse_launchd_target`] read back what an installed unit supervises, for
//! migration. No I/O — the bin crate writes and activates the rendered text.
//! Every interpolated path is escaped for its target format: XML entities in
//! the plist, double-quoted with `\`/`"`/`'` escapes plus `$$`/`%%`
//! expansion escapes in the systemd `ExecStart`.

use std::path::{Path, PathBuf};

/// launchd `Label` of the machine unit that runs `run --all` (#208).
pub const MACHINE_LABEL: &str = "sh.toolu.runner";
/// systemd user unit file name of the machine unit that runs `run --all`
/// (#208).
pub const MACHINE_UNIT: &str = "toolu-runner.service";

/// What an `install-service` invocation did, so the CLI can print the right
/// status line without re-inspecting the flags it passed in. The variants
/// map one-to-one to the command's outcomes (write+activate, write-only,
/// remove-hit, remove-miss, print).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAction {
  /// The unit file was written. `activated` is `true` when it was also loaded
  /// into the supervisor (default mode) and `false` for `--no-activate`.
  Installed {
    /// Whether the written unit was loaded into the supervisor.
    activated: bool,
  },
  /// `--remove` deleted an installed unit file.
  Removed,
  /// `--remove` found no unit file to delete (idempotent no-op).
  NothingToRemove,
  /// `--print` emitted the unit text to stdout; there is no status line.
  Printed,
}

/// The exact user-facing status line for a completed [`ServiceAction`], given
/// the service `label` and the acted-on unit `path`. Returns an empty string
/// for [`ServiceAction::Printed`] (print mode emits the unit text itself, not
/// a status line). The strings match `install-service`'s historical output
/// byte-for-byte, so the CLI never re-derives them from its flags.
pub fn service_action_summary(action: &ServiceAction, label: &str, path: &Path) -> String {
  match action {
    ServiceAction::Installed { activated: true } => {
      format!("installed {label} and activated it at {}", path.display())
    },
    ServiceAction::Installed { activated: false } => format!("wrote {}", path.display()),
    ServiceAction::Removed => format!("removed {label} ({})", path.display()),
    ServiceAction::NothingToRemove => {
      format!("no service installed at {} — nothing to do", path.display())
    },
    ServiceAction::Printed => String::new(),
  }
}

/// Inputs for supervisor-unit generation, shared by both renderers.
pub struct ServiceSpec<'a> {
  /// Supervisor identity: the launchd `Label` and the systemd
  /// `Description` suffix (e.g. `io.toolu.runner.<owner>.<repo>`).
  pub label: &'a str,
  /// Absolute path to the runner executable (`std::env::current_exe()`).
  pub exe: &'a Path,
  /// Absolute path to the registration's `config.toml`.
  pub config_path: &'a Path,
  /// `<data_dir>/_diag`; launchd's stdout/stderr logs land here.
  pub diag_dir: &'a Path,
  /// `PATH` baked into the launchd `EnvironmentVariables` dict — build it with
  /// [`launchd_env_path`]. Ignored by [`systemd_unit`]: the systemd user
  /// manager already exports a usable `PATH`, and overriding it would change
  /// behaviour for existing Linux installs.
  pub env_path: &'a str,
  /// launchd `WorkingDirectory` (the registration's `data_dir`). Without it a
  /// `LaunchAgent` starts in `/`. Ignored by [`systemd_unit`].
  pub work_dir: &'a Path,
}

/// Inputs for the machine unit: one supervisor for every registration under
/// the runner home (`run --all`).
pub struct MachineSpec<'a> {
  /// Absolute path to the runner executable (`std::env::current_exe()`).
  pub exe: &'a Path,
  /// The runner home to pin as `TOOLU_RUNNER_HOME` in the unit environment —
  /// `Some` only when the installing environment customised it; the default
  /// `~/.toolu-runner` resolves the same under the supervisor.
  pub home: Option<&'a Path>,
  /// `<home>/_diag`; launchd's stdout/stderr logs land here.
  pub diag_dir: &'a Path,
  /// `PATH` for the launchd `EnvironmentVariables` (see [`launchd_env_path`]).
  /// Ignored by [`systemd_machine_unit`], like [`ServiceSpec::env_path`].
  pub env_path: &'a str,
  /// launchd `WorkingDirectory` (the runner home). Ignored by
  /// [`systemd_machine_unit`].
  pub work_dir: &'a Path,
}

/// What an installed unit's command line supervises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Supervises {
  /// `run --all`: every registration under its runner home.
  All,
  /// `run --config <path>`: the one registration at `path`.
  Config(PathBuf),
}

/// The directories a `LaunchAgent` must have on `PATH` regardless of what the
/// installing shell had: Homebrew (Apple Silicon and Intel prefixes) plus the
/// system four launchd would otherwise supply on its own.
///
/// Public so the tests assert against THIS list rather than a copy that would
/// silently drift from it.
pub const GUARANTEED_PATH_DIRS: [&str; 7] = [
  "/opt/homebrew/bin",
  "/opt/homebrew/sbin",
  "/usr/local/bin",
  "/usr/bin",
  "/bin",
  "/usr/sbin",
  "/sbin",
];

/// Build the `PATH` to bake into a `LaunchAgent` from `current_path` — the
/// installing process's `PATH` (`None` when unset).
///
/// launchd gives an agent only `/usr/bin:/bin:/usr/sbin:/sbin`, so a
/// service-installed runner cannot see Homebrew — steps that work from a
/// terminal fail under the supervisor. The installing shell's entries come
/// first (that is the environment the operator verified by hand), then
/// [`GUARANTEED_PATH_DIRS`] fills in what is missing. Entries are deduplicated
/// keeping the FIRST occurrence, so the result is order-stable and applying
/// the function to its own output is a no-op. A nonexistent directory is
/// harmless — `execve` skips `PATH` entries that do not resolve — so the set
/// is appended unconditionally rather than probed, keeping the builder pure.
pub fn launchd_env_path(current_path: Option<&str>) -> String {
  let mut out: Vec<&str> = Vec::new();
  let entries = current_path
    .unwrap_or("")
    .split(':')
    .chain(GUARANTEED_PATH_DIRS);
  for entry in entries {
    if !entry.is_empty() && !out.contains(&entry) {
      out.push(entry);
    }
  }
  out.join(":")
}

/// Render a launchd user `LaunchAgent` plist for `spec` (all strings
/// XML-escaped). `KeepAlive` + `RunAtLoad` keep the runner up across
/// crashes and reboots; stdout/stderr go to `<diag_dir>/service.{out,err}.log`;
/// `WorkingDirectory` + an `EnvironmentVariables` `PATH` give job steps the
/// same tools an operator has in a terminal (see [`launchd_env_path`]).
pub fn launchd_plist(spec: &ServiceSpec<'_>) -> String {
  let args = [
    xml_path(spec.exe),
    "run".to_owned(),
    "--config".to_owned(),
    xml_path(spec.config_path),
  ];
  let env = [("PATH", xml_escape(spec.env_path))];
  render_plist(
    &xml_escape(spec.label),
    &args,
    &xml_path(spec.work_dir),
    &env,
    spec.diag_dir,
  )
}

/// Render the machine `LaunchAgent` ([`MACHINE_LABEL`]): the same shape as
/// [`launchd_plist`], running `<exe> run --all` in the runner home, with
/// `TOOLU_RUNNER_HOME` added to `EnvironmentVariables` when pinned.
pub fn launchd_machine_plist(spec: &MachineSpec<'_>) -> String {
  let args = [xml_path(spec.exe), "run".to_owned(), "--all".to_owned()];
  let mut env = vec![("PATH", xml_escape(spec.env_path))];
  if let Some(home) = spec.home {
    env.push(("TOOLU_RUNNER_HOME", xml_path(home)));
  }
  render_plist(
    MACHINE_LABEL,
    &args,
    &xml_path(spec.work_dir),
    &env,
    spec.diag_dir,
  )
}

/// The plist body shared by both renderers. Every string argument must
/// already be XML-escaped; `diag_dir` is escaped here.
fn render_plist(
  label: &str,
  args: &[String],
  work_dir: &str,
  env: &[(&str, String)],
  diag_dir: &Path,
) -> String {
  let out = xml_path(&diag_dir.join("service.out.log"));
  let err = xml_path(&diag_dir.join("service.err.log"));

  let mut s = String::new();
  s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
  s.push_str(
    "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
     \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
  );
  s.push_str("<plist version=\"1.0\">\n");
  s.push_str("<dict>\n");
  push_string_entry(&mut s, "  ", "Label", label);
  push_program_arguments(&mut s, args);
  push_string_entry(&mut s, "  ", "WorkingDirectory", work_dir);
  push_environment(&mut s, env);
  s.push_str("  <key>KeepAlive</key>\n");
  s.push_str("  <true/>\n");
  s.push_str("  <key>RunAtLoad</key>\n");
  s.push_str("  <true/>\n");
  push_string_entry(&mut s, "  ", "StandardOutPath", &out);
  push_string_entry(&mut s, "  ", "StandardErrorPath", &err);
  s.push_str("</dict>\n");
  s.push_str("</plist>\n");
  s
}

/// Push a `<key>`/`<string>` pair at `indent`. `value` must already be
/// XML-escaped — this helper does no escaping of its own.
fn push_string_entry(s: &mut String, indent: &str, key: &str, value: &str) {
  s.push_str(indent);
  s.push_str("<key>");
  s.push_str(key);
  s.push_str("</key>\n");
  s.push_str(indent);
  s.push_str("<string>");
  s.push_str(value);
  s.push_str("</string>\n");
}

/// Push the `ProgramArguments` array: the runner binary and its
/// (already XML-escaped) arguments, one `<string>` each.
fn push_program_arguments(s: &mut String, args: &[String]) {
  s.push_str("  <key>ProgramArguments</key>\n");
  s.push_str("  <array>\n");
  for arg in args {
    s.push_str("    <string>");
    s.push_str(arg);
    s.push_str("</string>\n");
  }
  s.push_str("  </array>\n");
}

/// Push the `EnvironmentVariables` dict (values already XML-escaped).
/// `PATH` is always first: launchd hands an agent a minimal one, and job
/// steps inherit whatever the runner process has.
fn push_environment(s: &mut String, env: &[(&str, String)]) {
  s.push_str("  <key>EnvironmentVariables</key>\n");
  s.push_str("  <dict>\n");
  for (key, value) in env {
    push_string_entry(s, "    ", key, value);
  }
  s.push_str("  </dict>\n");
}

/// Render a systemd user unit for `spec`. `ExecStart` double-quotes the exe
/// and config paths (so spaces survive) and `Restart=always` + `RestartSec=5`
/// give crash persistence, except `RestartPreventExitStatus=5` (a spent
/// session conflict, [`shared::SESSION_CONFLICT_EXIT_CODE`]) stays down;
/// `WantedBy=default.target` enables it at login.
pub fn systemd_unit(spec: &ServiceSpec<'_>) -> String {
  let exe = systemd_quote(spec.exe);
  let config = systemd_quote(spec.config_path);

  let mut s = String::new();
  s.push_str("[Unit]\n");
  // `%` is the only systemd-special character in a Description value
  // (specifier expansion, systemd.unit(5)); escape it as `%%`.
  s.push_str("Description=toolu-runner (");
  s.push_str(&spec.label.replace('%', "%%"));
  s.push_str(")\n");
  s.push('\n');
  s.push_str("[Service]\n");
  s.push_str("ExecStart=");
  s.push_str(&exe);
  s.push_str(" run --config ");
  s.push_str(&config);
  s.push('\n');
  s.push_str("Restart=always\n");
  s.push_str("RestartSec=5\n");
  // A session conflict that outlasted its bounded retry is final (upstream's
  // service stops on it too); restarting would make the retry unbounded.
  s.push_str("RestartPreventExitStatus=");
  s.push_str(&shared::SESSION_CONFLICT_EXIT_CODE.to_string());
  s.push('\n');
  s.push('\n');
  s.push_str("[Install]\n");
  s.push_str("WantedBy=default.target\n");
  s
}

/// Render the machine systemd user unit ([`MACHINE_UNIT`]): `ExecStart`
/// runs `<exe> run --all`, `ExecReload` sends SIGHUP so
/// `systemctl --user reload toolu-runner` rescans the registrations, and
/// `Restart=always` keeps it up. Unlike [`systemd_unit`] there is no
/// `RestartPreventExitStatus`: `run --all` never exits with a registration's
/// session-conflict code. `Environment=` pins `TOOLU_RUNNER_HOME` only when
/// [`MachineSpec::home`] is set.
pub fn systemd_machine_unit(spec: &MachineSpec<'_>) -> String {
  let mut s = String::new();
  s.push_str("[Unit]\n");
  s.push_str("Description=toolu-runner (");
  s.push_str(MACHINE_LABEL);
  s.push_str(")\n");
  s.push('\n');
  s.push_str("[Service]\n");
  if let Some(home) = spec.home {
    s.push_str("Environment=");
    s.push_str(&systemd_env_assignment("TOOLU_RUNNER_HOME", home));
    s.push('\n');
  }
  s.push_str("ExecStart=");
  s.push_str(&systemd_quote(spec.exe));
  s.push_str(" run --all\n");
  s.push_str("ExecReload=/bin/kill -HUP $MAINPID\n");
  s.push_str("Restart=always\n");
  s.push_str("RestartSec=5\n");
  s.push('\n');
  s.push_str("[Install]\n");
  s.push_str("WantedBy=default.target\n");
  s
}

/// A double-quoted systemd `Environment=` assignment. Inside the quotes
/// `\\` and `"` need C escapes and `%` specifier expansion still runs, so
/// `%` becomes `%%`; `$` is literal there (no variable expansion).
fn systemd_env_assignment(key: &str, value: &Path) -> String {
  let raw = format!("{key}={}", value.display());
  let escaped = raw
    .replace('\\', "\\\\")
    .replace('"', "\\\"")
    .replace('%', "%%");
  format!("\"{escaped}\"")
}

/// What the unit text `unit` supervises, from its `ExecStart=` command line
/// (systemd quoting and `$$` / `%%` escapes undone). `None` when there is no
/// `ExecStart=` or it is not a toolu `run --all` / `run --config <path>`.
pub fn parse_systemd_target(unit: &str) -> Option<Supervises> {
  let exec = unit
    .lines()
    .find_map(|line| line.trim().strip_prefix("ExecStart="))?;
  target_from_args(&systemd_words(exec))
}

/// What the plist text `plist` supervises, from its `ProgramArguments`
/// array (XML entities undone). `None` when there is no such array or it is
/// not a toolu `run --all` / `run --config <path>`.
pub fn parse_launchd_target(plist: &str) -> Option<Supervises> {
  let (_, after) = plist.split_once("<key>ProgramArguments</key>")?;
  let (_, array) = after.split_once("<array>")?;
  let (array, _) = array.split_once("</array>")?;
  let args: Vec<String> = array
    .split("<string>")
    .skip(1)
    .filter_map(|chunk| chunk.split_once("</string>"))
    .map(|(value, _)| xml_unescape(value))
    .collect();
  target_from_args(&args)
}

/// The `TOOLU_RUNNER_HOME` a systemd unit pins in an `Environment=` line
/// (quoting, `\\` escapes and `%%` undone), or `None` when it pins none.
pub fn parse_systemd_home(unit: &str) -> Option<PathBuf> {
  unit
    .lines()
    .filter_map(|line| line.trim().strip_prefix("Environment="))
    .flat_map(systemd_words)
    .find_map(|word| word.strip_prefix("TOOLU_RUNNER_HOME=").map(PathBuf::from))
}

/// The `TOOLU_RUNNER_HOME` a plist pins in `EnvironmentVariables`, or
/// `None` when it pins none.
pub fn parse_launchd_home(plist: &str) -> Option<PathBuf> {
  let (_, after) = plist.split_once("<key>TOOLU_RUNNER_HOME</key>")?;
  let (_, value) = after.split_once("<string>")?;
  let (value, _) = value.split_once("</string>")?;
  Some(PathBuf::from(xml_unescape(value)))
}

/// `run --all` → [`Supervises::All`]; `run --config <p>` (or
/// `--config=<p>`) → [`Supervises::Config`]; anything else → `None`.
fn target_from_args(args: &[String]) -> Option<Supervises> {
  let run = args.iter().position(|arg| arg == "run")?;
  let rest = args.get(run + 1..)?;
  if rest.iter().any(|arg| arg == "--all") {
    return Some(Supervises::All);
  }
  let mut iter = rest.iter();
  while let Some(arg) = iter.next() {
    if arg == "--config" {
      return iter
        .next()
        .map(|path| Supervises::Config(PathBuf::from(path)));
    }
    if let Some(path) = arg.strip_prefix("--config=") {
      return Some(Supervises::Config(PathBuf::from(path)));
    }
  }
  None
}

/// Split a systemd command line into words: whitespace separates words
/// outside double quotes, `\\` escapes the next character, and `$$` / `%%`
/// collapse to `$` / `%` — the inverse of [`systemd_quote`].
fn systemd_words(line: &str) -> Vec<String> {
  let mut words = Vec::new();
  let mut word = String::new();
  let mut in_word = false;
  let mut quoted = false;
  let mut chars = line.chars().peekable();
  while let Some(c) = chars.next() {
    match c {
      '"' => {
        quoted = !quoted;
        in_word = true;
      },
      '\\' => {
        if let Some(next) = chars.next() {
          word.push(next);
        }
        in_word = true;
      },
      '$' | '%' if chars.peek() == Some(&c) => {
        chars.next();
        word.push(c);
        in_word = true;
      },
      c if c.is_whitespace() && !quoted => {
        if in_word {
          words.push(std::mem::take(&mut word));
          in_word = false;
        }
      },
      c => {
        word.push(c);
        in_word = true;
      },
    }
  }
  if in_word {
    words.push(word);
  }
  words
}

/// Undo [`xml_escape`] (`&amp;` last, so `&amp;lt;` stays `&lt;`).
fn xml_unescape(s: &str) -> String {
  s.replace("&lt;", "<")
    .replace("&gt;", ">")
    .replace("&quot;", "\"")
    .replace("&apos;", "'")
    .replace("&amp;", "&")
}

/// XML-escape the five markup-significant characters (`&`, `<`, `>`, `"`,
/// `'`). `&` is replaced first so the other entities are not double-escaped.
fn xml_escape(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  for c in s.chars() {
    match c {
      '&' => out.push_str("&amp;"),
      '<' => out.push_str("&lt;"),
      '>' => out.push_str("&gt;"),
      '"' => out.push_str("&quot;"),
      '\'' => out.push_str("&apos;"),
      other => out.push(other),
    }
  }
  out
}

/// XML-escape a path's display form for use inside a plist `<string>`.
fn xml_path(p: &Path) -> String {
  let display = format!("{}", p.display());
  xml_escape(&display)
}

/// Double-quote a path for a systemd `ExecStart`, escaping `\`, `"`, and
/// `'` (C-style) so spaces and quotes survive systemd's tokenizer, plus
/// `$` → `$$` and `%` → `%%` — variable and specifier expansion run inside
/// double quotes too, so an unescaped `$`/`%` would be substituted.
fn systemd_quote(p: &Path) -> String {
  let raw = format!("{}", p.display());
  let escaped = raw
    .replace('\\', "\\\\")
    .replace('"', "\\\"")
    .replace('\'', "\\'")
    .replace('$', "$$")
    .replace('%', "%%");
  format!("\"{escaped}\"")
}
