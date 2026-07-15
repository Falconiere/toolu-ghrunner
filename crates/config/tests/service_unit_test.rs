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
  assert_eq!(plist, include_str!("fixtures/service/launchd_plain.plist"));
}

#[test]
fn systemd_unit_minimal_spec_without_spaces() {
  let unit = service_unit::systemd_unit(&plain_spec());
  assert_eq!(unit, include_str!("fixtures/service/systemd_plain.service"));
}

#[test]
fn systemd_unit_escapes_single_quote_in_paths() {
  // systemd's tokenizer treats `'` as a quoting character and its C-style
  // unescape accepts `\'` — a path carrying one must render escaped.
  let spec = ServiceSpec {
    label: "io.toolu.runner.octocat.hello",
    exe: Path::new("/opt/o'brien/toolu-runner"),
    config_path: Path::new("/home/ci/.toolu-runner/runners/octocat/hello/config.toml"),
    diag_dir: Path::new("/home/ci/.toolu-runner/runners/octocat/hello/_diag"),
  };
  let unit = service_unit::systemd_unit(&spec);
  assert_eq!(
    unit,
    include_str!("fixtures/service/systemd_squote.service")
  );
}

#[test]
fn systemd_unit_escapes_dollar_and_percent_in_paths() {
  // `$` (variable expansion) and `%` (specifier expansion) are substituted
  // inside double-quoted ExecStart values — they must render as `$$`/`%%`.
  let spec = ServiceSpec {
    label: "io.toolu.runner.octocat.hello",
    exe: Path::new("/opt/100% $rusty/toolu-runner"),
    config_path: Path::new("/home/ci/.toolu-runner/runners/octocat/hello/config.toml"),
    diag_dir: Path::new("/home/ci/.toolu-runner/runners/octocat/hello/_diag"),
  };
  let unit = service_unit::systemd_unit(&spec);
  assert_eq!(
    unit,
    include_str!("fixtures/service/systemd_specials.service")
  );
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
  assert_eq!(
    unit,
    include_str!("fixtures/service/systemd_percent.service")
  );
}
