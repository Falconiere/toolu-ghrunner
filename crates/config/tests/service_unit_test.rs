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

use std::path::{Path, PathBuf};

use config::service_unit::{
  self, GUARANTEED_PATH_DIRS, MachineSpec, ServiceSpec, Supervises, launchd_env_path,
};

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

/// The spaces spec exercises `env_path` escaping in both unit formats.
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
const PLAIN_CONFIG: &str = "/home/ci/.toolu-runner/runners/octocat/hello/config.toml";

/// Build the canonical space-free spec both minimal tests render.
fn plain_spec() -> ServiceSpec<'static> {
  ServiceSpec {
    label: "io.toolu.runner.octocat.hello",
    exe: Path::new("/usr/local/bin/toolu-runner"),
    config_path: Path::new(PLAIN_CONFIG),
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

/// Systemd gets the installing PATH; `WorkingDirectory` remains launchd-only.
#[test]
fn systemd_unit_declares_the_installing_path() {
  let mut altered = plain_spec();
  altered.env_path = "/totally/different/bin";
  altered.work_dir = Path::new("/totally/different/dir");
  let changed = service_unit::systemd_unit(&altered);
  let path_lines: Vec<_> = changed
    .lines()
    .filter(|line| line.starts_with("Environment=\"PATH="))
    .collect();
  assert_eq!(path_lines, ["Environment=\"PATH=/totally/different/bin\""]);
  assert!(!changed.contains("WorkingDirectory="));
}

#[test]
fn systemd_path_assignment_escapes_quotes_backslashes_and_percent() {
  let spec = ServiceSpec {
    env_path: "/opt/a b/\"quoted\"/100%/back\\slash:$literal",
    ..plain_spec()
  };
  let unit = service_unit::systemd_unit(&spec);
  assert_eq!(
    unit
      .lines()
      .filter(|line| line.starts_with("Environment="))
      .collect::<Vec<_>>(),
    ["Environment=\"PATH=/opt/a b/\\\"quoted\\\"/100%%/back\\\\slash:$literal\""]
  );
}

#[test]
fn systemd_unit_escapes_single_quote_in_paths() {
  // systemd's tokenizer treats `'` as a quoting character and its C-style
  // unescape accepts `\'` — a path carrying one must render escaped.
  let spec = ServiceSpec {
    label: "io.toolu.runner.octocat.hello",
    exe: Path::new("/opt/o'brien/toolu-runner"),
    config_path: Path::new(PLAIN_CONFIG),
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
    config_path: Path::new(PLAIN_CONFIG),
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

/// The static unit `install.sh --service` installs wraps the same `run` loop,
/// so a session conflict that outlasted its bounded retry (exit 5, #91) must
/// not be restarted there either.
#[test]
fn legacy_systemd_unit_does_not_restart_a_session_conflict() {
  let unit = include_str!("../../../scripts/toolu-runner.service");
  assert!(
    unit
      .lines()
      .any(|line| line == "RestartPreventExitStatus=5"),
    "scripts/toolu-runner.service must carry RestartPreventExitStatus=5"
  );
}

// ── the machine unit (`run --all`, #208) ────────────────────────────

const MACHINE_EXE: &str = "/Users/dev name/apps/toolu & runner/toolu-runner";
const MACHINE_ENV_PATH: &str = "/opt/dev tools & bin:/usr/bin:/bin";
/// A pinned home carrying a space, `%` and `"` — systemd `Environment=`
/// escaping plus plist XML escaping.
const MACHINE_HOME: &str = "/Users/dev name/100% \"toolu\" home";
const MACHINE_DIAG: &str = "/Users/dev name/100% \"toolu\" home/_diag";
const DEFAULT_HOME: &str = "/Users/dev name/.toolu-runner";
const DEFAULT_DIAG: &str = "/Users/dev name/.toolu-runner/_diag";

/// The machine spec: `home` is pinned in the unit env only when `pinned`;
/// `work_dir` and `diag_dir` always follow the runner home.
fn machine_spec(pinned: bool) -> MachineSpec<'static> {
  let (home, diag) = if pinned {
    (MACHINE_HOME, MACHINE_DIAG)
  } else {
    (DEFAULT_HOME, DEFAULT_DIAG)
  };
  MachineSpec {
    exe: Path::new(MACHINE_EXE),
    home: pinned.then(|| Path::new(home)),
    diag_dir: Path::new(diag),
    env_path: MACHINE_ENV_PATH,
    work_dir: Path::new(home),
  }
}

#[test]
fn systemd_machine_unit_matches_fixture() {
  let unit = service_unit::systemd_machine_unit(&machine_spec(false));
  assert_eq!(unit, include_str!("fixtures/service/machine_plain.service"));
}

#[test]
fn systemd_machine_unit_pins_a_custom_home_escaped() {
  let unit = service_unit::systemd_machine_unit(&machine_spec(true));
  assert_eq!(unit, include_str!("fixtures/service/machine_home.service"));
}

#[test]
fn systemd_system_machine_unit_uses_multi_user_target() {
  let unit = service_unit::systemd_system_machine_unit(&machine_spec(true), Path::new("/root"));
  assert_eq!(unit, include_str!("fixtures/service/machine_root.service"));
}

#[test]
fn systemd_system_registration_unit_uses_multi_user_target() {
  let unit = service_unit::systemd_system_unit(
    &plain_spec(),
    Path::new("/root"),
    Path::new("/home/ci/.toolu-runner"),
  );
  assert_eq!(unit, include_str!("fixtures/service/systemd_root.service"));
}

#[test]
fn systemd_user_registration_unit_pins_a_custom_runner_home() {
  let unit =
    service_unit::systemd_unit_with_home(&plain_spec(), Path::new("/home/ci/custom runner home"));
  assert!(
    unit
      .lines()
      .any(|line| line == "Environment=\"TOOLU_RUNNER_HOME=/home/ci/custom runner home\"")
  );
}

#[test]
fn systemd_system_machine_unit_pins_the_default_runner_home() {
  let spec = MachineSpec {
    home: Some(Path::new(DEFAULT_HOME)),
    ..machine_spec(false)
  };
  let unit = service_unit::systemd_system_machine_unit(&spec, Path::new("/root"));
  let environment = unit
    .lines()
    .filter(|line| line.starts_with("Environment="))
    .collect::<Vec<_>>();
  assert_eq!(
    environment,
    [
      "Environment=\"PATH=/opt/dev tools & bin:/usr/bin:/bin\"",
      "Environment=\"HOME=/root\"",
      &format!("Environment=\"TOOLU_RUNNER_HOME={DEFAULT_HOME}\""),
    ]
  );
}

#[test]
fn launchd_machine_plist_matches_fixture() {
  let plist = service_unit::launchd_machine_plist(&machine_spec(false));
  assert_eq!(plist, include_str!("fixtures/service/machine_plain.plist"));
}

#[test]
fn launchd_machine_plist_pins_a_custom_home_escaped() {
  let plist = service_unit::launchd_machine_plist(&machine_spec(true));
  assert_eq!(plist, include_str!("fixtures/service/machine_home.plist"));
}

/// The machine identity is distinct from every per-registration identity
/// and from the static `install.sh` agent's label.
#[test]
fn machine_identity_is_sh_toolu_runner_and_toolu_runner_service() {
  assert_eq!(service_unit::MACHINE_LABEL, "sh.toolu.runner");
  assert_eq!(service_unit::MACHINE_UNIT, "toolu-runner.service");
}

// ── what an installed unit supervises (migration, #208) ─────────────

#[test]
fn parse_targets_of_every_per_registration_fixture() {
  let cases = [
    (
      include_str!("fixtures/service/systemd_spaces.service"),
      SPACES_CONFIG,
    ),
    (
      include_str!("fixtures/service/systemd_plain.service"),
      PLAIN_CONFIG,
    ),
  ];
  for (unit, config) in cases {
    assert_eq!(
      service_unit::parse_systemd_target(unit),
      Some(Supervises::Config(PathBuf::from(config)))
    );
  }
  for (plist, config) in [
    (
      include_str!("fixtures/service/launchd_spaces.plist"),
      SPACES_CONFIG,
    ),
    (
      include_str!("fixtures/service/launchd_plain.plist"),
      PLAIN_CONFIG,
    ),
  ] {
    assert_eq!(
      service_unit::parse_launchd_target(plist),
      Some(Supervises::Config(PathBuf::from(config)))
    );
  }
}

/// Every escape the renderer emits is undone: a config path with `'`, `$`,
/// `%` and `"` round-trips through the rendered unit.
#[test]
fn parse_systemd_target_undoes_the_renderer_escapes() {
  let config = Path::new("/home/o'neil/$HOME/100%/\"q\"/config.toml");
  let spec = ServiceSpec {
    config_path: config,
    ..plain_spec()
  };
  let unit = service_unit::systemd_unit(&spec);
  assert_eq!(
    service_unit::parse_systemd_target(&unit),
    Some(Supervises::Config(config.to_path_buf()))
  );
  let plist = service_unit::launchd_plist(&spec);
  assert_eq!(
    service_unit::parse_launchd_target(&plist),
    Some(Supervises::Config(config.to_path_buf()))
  );
}

#[test]
fn parse_targets_of_the_machine_fixtures_are_all() {
  for unit in [
    include_str!("fixtures/service/machine_plain.service"),
    include_str!("fixtures/service/machine_home.service"),
  ] {
    assert_eq!(
      service_unit::parse_systemd_target(unit),
      Some(Supervises::All)
    );
  }
  for plist in [
    include_str!("fixtures/service/machine_plain.plist"),
    include_str!("fixtures/service/machine_home.plist"),
  ] {
    assert_eq!(
      service_unit::parse_launchd_target(plist),
      Some(Supervises::All)
    );
  }
}

/// The static `install.sh --service` units hard-code their config path,
/// unquoted.
#[test]
fn parse_targets_of_the_static_install_sh_units() {
  assert_eq!(
    service_unit::parse_systemd_target(include_str!("../../../scripts/toolu-runner.service")),
    Some(Supervises::Config(PathBuf::from(
      "/etc/toolu-runner/config.toml"
    )))
  );
  assert_eq!(
    service_unit::parse_launchd_target(include_str!("../../../scripts/io.toolu-runner.plist")),
    Some(Supervises::Config(PathBuf::from(
      "/Users/Shared/toolu-runner/config.toml"
    )))
  );
}

#[test]
fn parse_target_of_a_unit_that_runs_nothing_is_none() {
  assert_eq!(
    service_unit::parse_systemd_target("[Unit]\nDescription=x\n\n[Service]\nRestart=always\n"),
    None
  );
  assert_eq!(
    service_unit::parse_systemd_target("[Service]\nExecStart=/usr/bin/true\n"),
    None
  );
  assert_eq!(
    service_unit::parse_launchd_target("<plist><dict/></plist>"),
    None
  );
}

/// A machine unit's pinned home reads back exactly (escapes undone); an
/// unpinned one pins none.
#[test]
fn parse_the_pinned_home_of_machine_units() {
  assert_eq!(
    service_unit::parse_systemd_home(include_str!("fixtures/service/machine_home.service")),
    Some(PathBuf::from(MACHINE_HOME))
  );
  assert_eq!(
    service_unit::parse_launchd_home(include_str!("fixtures/service/machine_home.plist")),
    Some(PathBuf::from(MACHINE_HOME))
  );
  assert_eq!(
    service_unit::parse_systemd_home(include_str!("fixtures/service/machine_plain.service")),
    None
  );
  assert_eq!(
    service_unit::parse_launchd_home(include_str!("fixtures/service/machine_plain.plist")),
    None
  );
}
