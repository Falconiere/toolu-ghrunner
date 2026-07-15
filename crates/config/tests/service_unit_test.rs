//! Exact-match tests for `config::service_unit` (always-online AC-8).
//!
//! Real known-good fixtures: `launchd_plist` / `systemd_unit` are rendered
//! for a representative spec whose exe/config/diag paths contain spaces and
//! an `&`, then compared byte-for-byte against committed fixtures — proving
//! XML-entity (plist) and double-quote (systemd) escaping. A second,
//! space-free spec covers the plain path shape.

use std::path::Path;

use config::service_unit::{self, ServiceSpec};

// ── the spaces + ampersand spec, exact-matched against fixtures ──────

/// A spec whose every path carries spaces and an `&` component, forcing
/// both XML escaping (`&` → `&amp;`) and systemd double-quoting.
const SPACES_LABEL: &str = "io.toolu.runner.a & b.repo";
const SPACES_EXE: &str = "/Users/dev name/apps/toolu & runner/toolu-runner";
const SPACES_CONFIG: &str = "/Users/dev name/.toolu-runner/runners/a & b/repo/config.toml";
const SPACES_DIAG: &str = "/Users/dev name/.toolu-runner/runners/a & b/repo/_diag";

#[test]
fn launchd_plist_matches_fixture_with_spaces_and_ampersand() {
  let spec = ServiceSpec {
    label: SPACES_LABEL,
    exe: Path::new(SPACES_EXE),
    config_path: Path::new(SPACES_CONFIG),
    diag_dir: Path::new(SPACES_DIAG),
  };
  let rendered = service_unit::launchd_plist(&spec);
  assert_eq!(
    rendered,
    include_str!("fixtures/service/launchd_spaces.plist")
  );
}

#[test]
fn systemd_unit_matches_fixture_with_spaces_and_ampersand() {
  let spec = ServiceSpec {
    label: SPACES_LABEL,
    exe: Path::new(SPACES_EXE),
    config_path: Path::new(SPACES_CONFIG),
    diag_dir: Path::new(SPACES_DIAG),
  };
  let rendered = service_unit::systemd_unit(&spec);
  assert_eq!(
    rendered,
    include_str!("fixtures/service/systemd_spaces.service")
  );
}

// ── a minimal, space-free spec (plain path shape) ───────────────────

/// Build the canonical space-free spec both minimal tests render.
fn plain_spec() -> ServiceSpec<'static> {
  ServiceSpec {
    label: "io.toolu.runner.octocat.hello",
    exe: Path::new("/usr/local/bin/toolu-runner"),
    config_path: Path::new("/home/ci/.toolu-runner/runners/octocat/hello/config.toml"),
    diag_dir: Path::new("/home/ci/.toolu-runner/runners/octocat/hello/_diag"),
  }
}

#[test]
fn launchd_plist_minimal_spec_without_spaces() {
  let plist = service_unit::launchd_plist(&plain_spec());
  assert!(plist.contains("<key>Label</key>\n  <string>io.toolu.runner.octocat.hello</string>\n"));
  assert!(plist.contains("<string>/usr/local/bin/toolu-runner</string>\n"));
  assert!(plist.contains("<string>run</string>\n"));
  assert!(plist.contains("<string>--config</string>\n"));
  assert!(
    plist.contains("<string>/home/ci/.toolu-runner/runners/octocat/hello/config.toml</string>\n")
  );
  assert!(plist.contains("<key>KeepAlive</key>\n  <true/>\n"));
  assert!(plist.contains("<key>RunAtLoad</key>\n  <true/>\n"));
  assert!(plist.contains(
    "<string>/home/ci/.toolu-runner/runners/octocat/hello/_diag/service.out.log</string>\n"
  ));
  assert!(plist.contains(
    "<string>/home/ci/.toolu-runner/runners/octocat/hello/_diag/service.err.log</string>\n"
  ));
  // No spaces means no `&amp;` should ever appear.
  assert!(!plist.contains("&amp;"));
}

#[test]
fn systemd_unit_minimal_spec_without_spaces() {
  let unit = service_unit::systemd_unit(&plain_spec());
  assert!(unit.contains("Description=toolu-runner (io.toolu.runner.octocat.hello)\n"));
  assert!(unit.contains(
    "ExecStart=\"/usr/local/bin/toolu-runner\" run --config \
     \"/home/ci/.toolu-runner/runners/octocat/hello/config.toml\"\n"
  ));
  assert!(unit.contains("Restart=always\n"));
  assert!(unit.contains("RestartSec=5\n"));
  assert!(unit.contains("WantedBy=default.target\n"));
}

#[test]
fn systemd_unit_escapes_percent_in_description() {
  // `%` is systemd's specifier character (systemd.unit(5)) — a label
  // carrying one must render as `%%` in Description.
  let spec = ServiceSpec {
    label: "io.toolu.runner.100%.repo",
    exe: Path::new("/usr/local/bin/toolu-runner"),
    config_path: Path::new("/home/ci/.toolu-runner/runners/pct/repo/config.toml"),
    diag_dir: Path::new("/home/ci/.toolu-runner/runners/pct/repo/_diag"),
  };
  let unit = service_unit::systemd_unit(&spec);
  assert!(unit.contains("Description=toolu-runner (io.toolu.runner.100%%.repo)\n"));
}
