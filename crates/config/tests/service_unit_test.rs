//! Exact-match tests for `config::service_unit` (always-online AC-8).
//!
//! Real known-good fixtures: `launchd_plist` / `systemd_unit` are rendered
//! for a representative spec whose exe/config/diag paths contain spaces and
//! an `&`, then compared byte-for-byte against committed fixtures — proving
//! XML-entity (plist) and double-quote (systemd) escaping. A second,
//! space-free spec covers the plain path shape.
//!
//! `launchd_env_path` is exercised against this test process's REAL `PATH`
//! (that is the input the installing shell supplies), not a synthetic string.

use std::path::Path;

use config::service_unit::{self, GUARANTEED_PATH_DIRS, ServiceSpec, launchd_env_path};

// ── the spaces + ampersand spec, exact-matched against fixtures ──────

/// A spec whose every path carries spaces and an `&` component, forcing
/// both XML escaping (`&` → `&amp;`) and systemd double-quoting.
const SPACES_LABEL: &str = "io.toolu.runner.a & b.repo";
const SPACES_EXE: &str = "/Users/dev name/apps/toolu & runner/toolu-runner";
const SPACES_CONFIG: &str = "/Users/dev name/.toolu-runner/runners/a & b/repo/config.toml";
const SPACES_DIAG: &str = "/Users/dev name/.toolu-runner/runners/a & b/repo/_diag";
const SPACES_WORK: &str = "/Users/dev name/.toolu-runner/runners/a & b/repo";
/// Carries a space and an `&` so the fixture also pins `env_path` escaping.
const SPACES_ENV_PATH: &str = "/opt/dev tools & bin:/usr/bin:/bin";

/// The spaces spec. `env_path`/`work_dir` are only read by `launchd_plist`;
/// the systemd test renders the same spec to prove they are ignored there.
fn spaces_spec() -> ServiceSpec<'static> {
  ServiceSpec {
    label: SPACES_LABEL,
    exe: Path::new(SPACES_EXE),
    config_path: Path::new(SPACES_CONFIG),
    diag_dir: Path::new(SPACES_DIAG),
    env_path: SPACES_ENV_PATH,
    work_dir: Path::new(SPACES_WORK),
  }
}

#[test]
fn launchd_plist_matches_fixture_with_spaces_and_ampersand() {
  let rendered = service_unit::launchd_plist(&spaces_spec());
  assert_eq!(
    rendered,
    include_str!("fixtures/service/launchd_spaces.plist")
  );
}

#[test]
fn systemd_unit_matches_fixture_with_spaces_and_ampersand() {
  let rendered = service_unit::systemd_unit(&spaces_spec());
  assert_eq!(
    rendered,
    include_str!("fixtures/service/systemd_spaces.service")
  );
}

// ── a minimal, space-free spec (plain path shape) ───────────────────

const PLAIN_WORK: &str = "/home/ci/.toolu-runner/runners/octocat/hello";
const PLAIN_ENV_PATH: &str = "/opt/homebrew/bin:/usr/bin:/bin";

/// Build the canonical space-free spec both minimal tests render.
fn plain_spec() -> ServiceSpec<'static> {
  ServiceSpec {
    label: "io.toolu.runner.octocat.hello",
    exe: Path::new("/usr/local/bin/toolu-runner"),
    config_path: Path::new("/home/ci/.toolu-runner/runners/octocat/hello/config.toml"),
    diag_dir: Path::new("/home/ci/.toolu-runner/runners/octocat/hello/_diag"),
    env_path: PLAIN_ENV_PATH,
    work_dir: Path::new(PLAIN_WORK),
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

/// The systemd renderer must ignore `env_path`/`work_dir` entirely — the
/// systemd user manager already exports a usable PATH, and overriding it would
/// change behaviour for existing Linux installs (spec non-goal 4).
#[test]
fn systemd_unit_ignores_the_launchd_only_fields() {
  let baseline = service_unit::systemd_unit(&plain_spec());
  let mut altered = plain_spec();
  altered.env_path = "/totally/different/bin";
  altered.work_dir = Path::new("/totally/different/dir");
  assert_eq!(service_unit::systemd_unit(&altered), baseline);
  assert!(!baseline.contains("Environment="));
  assert!(!baseline.contains("WorkingDirectory="));
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
    env_path: PLAIN_ENV_PATH,
    work_dir: Path::new(PLAIN_WORK),
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
    env_path: PLAIN_ENV_PATH,
    work_dir: Path::new(PLAIN_WORK),
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
    env_path: PLAIN_ENV_PATH,
    work_dir: Path::new(PLAIN_WORK),
  };
  let unit = service_unit::systemd_unit(&spec);
  assert_eq!(
    unit,
    include_str!("fixtures/service/systemd_percent.service")
  );
}

// ── launchd_env_path, against this process's real PATH ──────────────

/// Every guaranteed directory is present, and every entry the real installing
/// shell had survives — a service-run step must not lose tooling the operator
/// could see.
#[test]
fn launchd_env_path_keeps_the_real_path_and_adds_the_guaranteed_dirs() {
  let real = std::env::var("PATH").unwrap_or_else(|_| String::new());
  let built = launchd_env_path(Some(&real));
  let entries: Vec<&str> = built.split(':').collect();

  for dir in GUARANTEED_PATH_DIRS {
    assert!(entries.contains(&dir), "missing {dir} in {built}");
  }
  for entry in real.split(':').filter(|e| !e.is_empty()) {
    assert!(entries.contains(&entry), "dropped {entry} from {built}");
  }
}

/// First occurrence wins: no entry appears twice, whether the duplicate came
/// from the input itself or from the guaranteed set.
#[test]
fn launchd_env_path_deduplicates_keeping_first_occurrence() {
  let input = "/usr/bin:/opt/mine:/usr/bin:/opt/homebrew/bin:/opt/mine";
  let built = launchd_env_path(Some(input));
  let entries: Vec<&str> = built.split(':').collect();

  let mut seen = entries.clone();
  seen.sort_unstable();
  seen.dedup();
  assert_eq!(seen.len(), entries.len(), "duplicate entry in {built}");

  // Order of first occurrences is preserved, installing shell first.
  assert!(built.starts_with("/usr/bin:/opt/mine:/opt/homebrew/bin:"));
}

/// Applying the builder to its own output changes nothing — the value baked
/// into a plist survives a re-run of `install-service`.
#[test]
fn launchd_env_path_is_idempotent() {
  let real = std::env::var("PATH").unwrap_or_else(|_| String::new());
  let once = launchd_env_path(Some(&real));
  assert_eq!(launchd_env_path(Some(&once)), once);
}

/// An unset PATH must still yield a usable one — an empty `PATH` would leave
/// the service unable to spawn `bash` for a `run:` step.
#[test]
fn launchd_env_path_without_a_current_path_is_the_guaranteed_set() {
  assert_eq!(launchd_env_path(None), GUARANTEED_PATH_DIRS.join(":"));
  assert_eq!(launchd_env_path(Some("")), GUARANTEED_PATH_DIRS.join(":"));
}

/// Empty segments (a trailing `:` or `::`, both legal in a real PATH and
/// meaning "cwd") are dropped rather than baked into a service unit.
#[test]
fn launchd_env_path_drops_empty_segments() {
  let built = launchd_env_path(Some("/opt/mine::/usr/bin:"));
  assert!(
    !built.split(':').any(str::is_empty),
    "empty segment: {built}"
  );
  assert!(built.starts_with("/opt/mine:/usr/bin:"));
}
