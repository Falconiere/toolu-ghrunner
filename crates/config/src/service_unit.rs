//! Supervisor unit rendering for `install-service`.
//!
//! Pure string builders that turn a [`ServiceSpec`] into a launchd user
//! `LaunchAgent` plist (macOS) or a systemd user unit (Linux). No I/O — the
//! bin crate writes and activates the rendered text. Every interpolated
//! path is escaped for its target format: XML entities in the plist,
//! double-quoted with `\`/`"`/`'` escapes plus `$$`/`%%` expansion escapes
//! in the systemd `ExecStart`.

use std::path::Path;

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
  let label = xml_escape(spec.label);
  let exe = xml_path(spec.exe);
  let config = xml_path(spec.config_path);
  let out = xml_path(&spec.diag_dir.join("service.out.log"));
  let err = xml_path(&spec.diag_dir.join("service.err.log"));
  let work_dir = xml_path(spec.work_dir);
  let env_path = xml_escape(spec.env_path);

  let mut s = String::new();
  s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
  s.push_str(
    "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
     \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
  );
  s.push_str("<plist version=\"1.0\">\n");
  s.push_str("<dict>\n");
  push_string_entry(&mut s, "  ", "Label", &label);
  push_program_arguments(&mut s, &exe, &config);
  push_string_entry(&mut s, "  ", "WorkingDirectory", &work_dir);
  push_environment(&mut s, &env_path);
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

/// Push the `ProgramArguments` array: the runner binary invoked as
/// `run --config <path>`.
fn push_program_arguments(s: &mut String, exe: &str, config: &str) {
  s.push_str("  <key>ProgramArguments</key>\n");
  s.push_str("  <array>\n");
  s.push_str("    <string>");
  s.push_str(exe);
  s.push_str("</string>\n");
  s.push_str("    <string>run</string>\n");
  s.push_str("    <string>--config</string>\n");
  s.push_str("    <string>");
  s.push_str(config);
  s.push_str("</string>\n");
  s.push_str("  </array>\n");
}

/// Push the `EnvironmentVariables` dict. `PATH` is the only entry: launchd
/// hands an agent a minimal one, and job steps inherit whatever the runner
/// process has.
fn push_environment(s: &mut String, env_path: &str) {
  s.push_str("  <key>EnvironmentVariables</key>\n");
  s.push_str("  <dict>\n");
  push_string_entry(s, "    ", "PATH", env_path);
  s.push_str("  </dict>\n");
}

/// Render a systemd user unit for `spec`. `ExecStart` double-quotes the exe
/// and config paths (so spaces survive) and `Restart=always` + `RestartSec=5`
/// give crash persistence; `WantedBy=default.target` enables it at login.
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
  s.push('\n');
  s.push_str("[Install]\n");
  s.push_str("WantedBy=default.target\n");
  s
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
