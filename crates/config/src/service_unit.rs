//! Supervisor unit rendering for `install-service`.
//!
//! Pure string builders that turn a [`ServiceSpec`] into a launchd user
//! LaunchAgent plist (macOS) or a systemd user unit (Linux). No I/O — the
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
}

/// Render a launchd user LaunchAgent plist for `spec` (all strings
/// XML-escaped). `KeepAlive` + `RunAtLoad` keep the runner up across
/// crashes and reboots; stdout/stderr go to `<diag_dir>/service.{out,err}.log`.
pub fn launchd_plist(spec: &ServiceSpec<'_>) -> String {
  let label = xml_escape(spec.label);
  let exe = xml_path(spec.exe);
  let config = xml_path(spec.config_path);
  let out = xml_path(&spec.diag_dir.join("service.out.log"));
  let err = xml_path(&spec.diag_dir.join("service.err.log"));

  let mut s = String::new();
  s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
  s.push_str(
    "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
     \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
  );
  s.push_str("<plist version=\"1.0\">\n");
  s.push_str("<dict>\n");
  s.push_str("  <key>Label</key>\n");
  s.push_str(&format!("  <string>{label}</string>\n"));
  s.push_str("  <key>ProgramArguments</key>\n");
  s.push_str("  <array>\n");
  s.push_str(&format!("    <string>{exe}</string>\n"));
  s.push_str("    <string>run</string>\n");
  s.push_str("    <string>--config</string>\n");
  s.push_str(&format!("    <string>{config}</string>\n"));
  s.push_str("  </array>\n");
  s.push_str("  <key>KeepAlive</key>\n");
  s.push_str("  <true/>\n");
  s.push_str("  <key>RunAtLoad</key>\n");
  s.push_str("  <true/>\n");
  s.push_str("  <key>StandardOutPath</key>\n");
  s.push_str(&format!("  <string>{out}</string>\n"));
  s.push_str("  <key>StandardErrorPath</key>\n");
  s.push_str(&format!("  <string>{err}</string>\n"));
  s.push_str("</dict>\n");
  s.push_str("</plist>\n");
  s
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
  s.push_str(&format!(
    "Description=toolu-runner ({})\n",
    spec.label.replace('%', "%%")
  ));
  s.push('\n');
  s.push_str("[Service]\n");
  s.push_str(&format!("ExecStart={exe} run --config {config}\n"));
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
