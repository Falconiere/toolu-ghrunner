//! Tests for the `doctor` registration rows: credentials-mode classification,
//! the credentials / JIT / pending-removal rows against real files, the label,
//! and a real TOML parse error that must not echo a secret. Configs are
//! persisted and reloaded the way `register` does. The run-state and
//! last-job rows are tested in the sibling `doctor_registrations_state.rs`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use config::config::{
  CacheSection, CredentialsFile, RunnerRegistrationConfig, RuntimeConfig, ServicesSection,
  ShadowSection, WorkspaceSection, load_config, save_config, save_credentials,
};

use super::{
  check_registration, creds_mode_row, creds_row, jit_row, label, pending_row,
  without_quoted_strings, without_source_excerpt,
};
use crate::doctor_report::{Level, Row};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const CONFIG: &str = "/h/runners/o/r/config.toml";
const REREGISTER: &str = "re-register: toolu-runner register --replace --url <repo url>";
const GHES_JIT: &str = include_str!("../../tests/fixtures/jit_config_ghes.json");
const GITHUB_COM_JIT: &str = include_str!("../../tests/fixtures/jit_config_github_com.json");
const SECRET: &str = "s3cret";
/// What the TOML parser says about an unquoted string value.
const BAD_PROXY_MESSAGE: &str = "invalid string; expected `\"`, `'`";

/// The `encoded` envelope of a captured JIT config fixture.
fn encoded_jit(fixture: &str) -> Result<String, Box<dyn std::error::Error>> {
  let json: serde_json::Value = serde_json::from_str(fixture)?;
  Ok(
    json
      .get("encoded")
      .and_then(serde_json::Value::as_str)
      .ok_or("fixture has no encoded field")?
      .to_owned(),
  )
}

/// Persist a registration for `o/r` under `<home>/runners/` and load it back,
/// the way `run` sees it. Returns the loaded config and its `config.toml`.
fn persisted(
  home: &Path,
  jit_config: &str,
  ephemeral: bool,
) -> Result<(RunnerRegistrationConfig, PathBuf), Box<dyn std::error::Error>> {
  let dir = home.join("runners").join("o").join("r");
  let config_path = dir.join("config.toml");
  save_config(
    &config_path,
    &RunnerRegistrationConfig {
      runner_url: "https://ghes.example.com/o/r".to_owned(),
      runner_name: "doctor-unit".to_owned(),
      runner_id: 7,
      auth_token: "client-id".to_owned(),
      labels: vec!["self-hosted".to_owned()],
      runner_group: "Default".to_owned(),
      ephemeral,
      runtime: RuntimeConfig {
        jit_config: jit_config.to_owned(),
        work_dir: dir.join("_work").to_string_lossy().into_owned(),
        data_dir: dir.to_string_lossy().into_owned(),
        protocol_version: "v1".to_owned(),
      },
      services: ServicesSection::default(),
      cache: CacheSection::default(),
      workspace: WorkspaceSection::default(),
      shadow: ShadowSection::default(),
      network: config::network::NetworkSection::default(),
      auth: config::registration_auth::AuthSection::default(),
      provider: config::provider::ProviderSection::Github,
    },
  )?;
  Ok((load_config(&config_path)?, config_path))
}

/// A `config.toml` hand-edited with an UNQUOTED proxy URL holding a password:
/// the TOML parser quotes that very line in its error. Returns the path and
/// the 1-based line number of the bad `proxy` line.
fn config_with_unquoted_proxy(home: &Path) -> Result<(PathBuf, usize), Box<dyn std::error::Error>> {
  let (_, config_path) = persisted(home, &encoded_jit(GHES_JIT)?, false)?;
  let mut text = std::fs::read_to_string(&config_path)?;
  // The appended block is a blank line, `[network]`, then the proxy line.
  let proxy_line = text.lines().count() + 3;
  write!(
    text,
    "\n[network]\nproxy = http://user:{SECRET}@proxy.example:8080\n"
  )?;
  std::fs::write(&config_path, text)?;
  Ok((config_path, proxy_line))
}

#[test]
fn owner_only_credentials_pass_and_wider_modes_warn_with_chmod() {
  let path = Path::new("/h/runners/o/r/credentials.json");
  for (mode, shown) in [
    (0o100_600, "0600"),
    (0o100_400, "0400"),
    (0o100_700, "0700"),
  ] {
    assert_eq!(
      creds_mode_row(path, mode),
      Row::new(Level::Ok, "creds", format!("present ({shown})")),
      "{mode:o}"
    );
  }
  for (mode, shown) in [
    (0o100_644, "0644"),
    (0o100_640, "0640"),
    (0o100_604, "0604"),
    (0o100_660, "0660"),
    (0o100_777, "0777"),
  ] {
    assert_eq!(
      creds_mode_row(path, mode),
      Row::new(
        Level::Warn,
        "creds",
        format!("mode {shown}: readable beyond the owner")
      )
      .with_fix("chmod 600 /h/runners/o/r/credentials.json"),
      "{mode:o}"
    );
  }
}

#[test]
fn a_real_toml_parse_error_loses_the_quoted_source_line() -> TestResult {
  let dir = tempfile::tempdir()?;
  let (config_path, proxy_line) = config_with_unquoted_proxy(dir.path())?;
  let error = load_config(&config_path)
    .err()
    .ok_or("an unquoted proxy URL parsed")?;
  let raw = error.to_string();
  // The premise: the parser really does quote the secret-bearing line.
  assert!(raw.contains(SECRET), "raw error: {raw}");

  let cleaned = without_source_excerpt(&raw);
  assert!(
    cleaned.contains("TOML parse error at line"),
    "raw error: {raw}\ncleaned: {cleaned}"
  );
  assert!(
    !cleaned.contains(SECRET),
    "raw error: {raw}\ncleaned: {cleaned}"
  );
  assert!(
    !cleaned.contains('|'),
    "raw error: {raw}\ncleaned: {cleaned}"
  );
  // Location and message survive, joined on one line.
  assert_eq!(
    cleaned,
    format!(
      "config error: parse {}: TOML parse error at line {proxy_line}, column 9; {BAD_PROXY_MESSAGE}",
      config_path.display()
    ),
    "raw error: {raw}"
  );
  Ok(())
}

#[tokio::test]
async fn an_unparseable_config_fails_without_echoing_its_secret() -> TestResult {
  let dir = tempfile::tempdir()?;
  let (config_path, proxy_line) = config_with_unquoted_proxy(dir.path())?;

  let checked = check_registration(&config_path, Duration::from_secs(1)).await;
  assert_eq!(checked.heading, "  o/r:");
  assert!(checked.work_dir.is_none());
  assert!(!checked.unit_installed);
  assert!(checked.config.is_none());
  let shown = config_path.display();
  assert_eq!(
    checked.rows,
    vec![
      Row::new(
        Level::Fail,
        "config",
        format!(
          "cannot load {shown}: config error: parse {shown}: TOML parse error at line {proxy_line}, column 9; {BAD_PROXY_MESSAGE}"
        )
      )
      .with_fix(REREGISTER)
    ]
  );
  Ok(())
}

#[test]
fn a_missing_credentials_file_fails_naming_the_path() -> TestResult {
  let dir = tempfile::tempdir()?;
  let path = dir.path().join("credentials.json");
  assert_eq!(
    creds_row(&path),
    Row::new(Level::Fail, "creds", format!("missing {}", path.display())).with_fix(REREGISTER)
  );
  Ok(())
}

#[test]
fn garbage_credentials_fail_as_unreadable_with_the_parse_error() -> TestResult {
  let dir = tempfile::tempdir()?;
  let path = dir.path().join("credentials.json");
  let shown = path.display();

  std::fs::write(&path, "not json at all")?;
  assert_eq!(
    creds_row(&path),
    Row::new(
      Level::Fail,
      "creds",
      format!("unreadable: config error: parse {shown}: expected ident at line 1 column 2")
    )
    .with_fix(REREGISTER)
  );

  // Valid JSON of the wrong shape names the missing field.
  std::fs::write(&path, "{\"issued_at\": \"2026-10-19T00:00:00Z\"}")?;
  assert_eq!(
    creds_row(&path),
    Row::new(
      Level::Fail,
      "creds",
      format!(
        "unreadable: config error: parse {shown}: missing field `access_token` at line 1 column 37"
      )
    )
    .with_fix(REREGISTER)
  );

  // Bytes that are not UTF-8 never reach the JSON parser.
  std::fs::write(&path, [0xff_u8, 0xfe, 0x00])?;
  assert_eq!(
    creds_row(&path),
    Row::new(
      Level::Fail,
      "creds",
      format!("unreadable: config error: read {shown}: stream did not contain valid UTF-8")
    )
    .with_fix(REREGISTER)
  );
  Ok(())
}

#[cfg(unix)]
#[test]
fn valid_credentials_pass_at_owner_only_modes_and_warn_when_wider() -> TestResult {
  use std::os::unix::fs::PermissionsExt as _;

  let dir = tempfile::tempdir()?;
  let path = dir.path().join("credentials.json");
  save_credentials(
    &path,
    &CredentialsFile {
      access_token: "client-id".to_owned(),
      issued_at: "2026-10-19T00:00:00Z".to_owned(),
      expires_at: None,
    },
  )?;
  // `save_credentials` writes 0600.
  assert_eq!(
    creds_row(&path),
    Row::new(Level::Ok, "creds", "present (0600)")
  );

  std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))?;
  assert_eq!(
    creds_row(&path),
    Row::new(Level::Ok, "creds", "present (0400)")
  );

  std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
  assert_eq!(
    creds_row(&path),
    Row::new(Level::Warn, "creds", "mode 0644: readable beyond the owner")
      .with_fix(format!("chmod 600 {}", path.display()))
  );
  Ok(())
}

#[test]
fn a_garbage_jit_config_fails_and_the_real_envelopes_parse() -> TestResult {
  let dir = tempfile::tempdir()?;

  let (garbage, _) = persisted(dir.path(), "not-a-jit-envelope", false)?;
  assert_eq!(
    jit_row(&garbage),
    Row::new(
      Level::Fail,
      "jit",
      "unreadable: the registration's jit_config is not a valid JIT config envelope"
    )
    .with_fix(REREGISTER)
  );

  for fixture in [GHES_JIT, GITHUB_COM_JIT] {
    let encoded = encoded_jit(fixture)?;
    let (real, _) = persisted(dir.path(), &encoded, false)?;
    assert_eq!(
      jit_row(&real),
      Row::new(
        Level::Ok,
        "jit",
        format!("parses ({} bytes)", encoded.len())
      )
    );
  }
  Ok(())
}

#[test]
fn a_pending_removal_marker_warns_with_the_remove_command() -> TestResult {
  let dir = tempfile::tempdir()?;
  let config_path = Path::new(CONFIG);
  assert_eq!(pending_row(dir.path(), config_path), None);
  std::fs::write(dir.path().join(".pending_remove"), "")?;
  assert_eq!(
    pending_row(dir.path(), config_path),
    Some(
      Row::new(
        Level::Warn,
        "pending",
        "removal pending: `run` exits at its next job boundary"
      )
      .with_fix("toolu-runner remove --config /h/runners/o/r/config.toml")
    )
  );
  Ok(())
}

#[test]
fn the_label_is_owner_repo_under_runners_else_the_path() {
  assert_eq!(label(Path::new(CONFIG)), "o/r");
  assert_eq!(label(Path::new("/h/config.toml")), "/h/config.toml");
}

#[tokio::test]
async fn a_mistyped_secret_value_is_cut_from_the_config_row() -> TestResult {
  let dir = tempfile::tempdir()?;
  let (_, config_path) = persisted(dir.path(), &encoded_jit(GHES_JIT)?, false)?;
  let text = std::fs::read_to_string(&config_path)?;
  // `[network]` written as a bare string: serde quotes the value it rejects.
  std::fs::write(
    &config_path,
    format!("network = \"http://user:{SECRET}@proxy.example:8080\"\n{text}"),
  )?;
  let raw = load_config(&config_path)
    .err()
    .ok_or("a string [network] loaded")?
    .to_string();
  assert!(raw.contains(SECRET), "raw error: {raw}");

  let checked = check_registration(&config_path, Duration::from_secs(1)).await;
  assert_eq!(
    checked.rows,
    [Row::new(
      Level::Fail,
      "config",
      format!(
        "cannot load {path}: config error: parse {path}: TOML parse error at line 1, \
         column 11; invalid type: string \"…\", expected struct NetworkSection",
        path = config_path.display()
      ),
    )
    .with_fix("re-register: toolu-runner register --replace --url <repo url>")],
    "raw error: {raw}"
  );
  Ok(())
}

#[test]
fn quoted_string_values_are_cut_through_their_escapes() {
  assert_eq!(
    without_quoted_strings(r#"invalid type: string "a\"b:pw", expected x"#),
    r#"invalid type: string "…", expected x"#
  );
  assert_eq!(
    without_quoted_strings(r#"invalid value: string "one", or string "two""#),
    r#"invalid value: string "…", or string "…""#
  );
  // No closing quote: the remainder is dropped, never echoed.
  assert_eq!(
    without_quoted_strings(r#"invalid type: string "pw-unterminated"#),
    r#"invalid type: string "…""#
  );
  let untouched = "invalid string; expected `\"`, `'`";
  assert_eq!(without_quoted_strings(untouched), untouched);
}
