//! #201 AC-9: the `[auth]` section of `config.toml`.
//!
//! Real files only: registrations are written with `save_config` into a
//! temp dir and read back with `load_config`, and the hand-written tables are
//! the exact text an operator (or an older toolu) leaves on disk.

use config::config::{
  CacheSection, RunnerRegistrationConfig, RuntimeConfig, ServicesSection, ShadowSection,
  WorkspaceSection, load_config, save_config,
};
use config::network::NetworkSection;
use config::registration_auth::AuthSection;
use tempfile::TempDir;

fn registration(auth: AuthSection) -> RunnerRegistrationConfig {
  RunnerRegistrationConfig {
    runner_url: "https://github.com/octo/demo".to_owned(),
    runner_name: "runner-201".to_owned(),
    runner_id: 7,
    auth_token: "Iv1.client".to_owned(),
    labels: vec!["self-hosted".to_owned()],
    runner_group: "Default".to_owned(),
    ephemeral: false,
    runtime: RuntimeConfig {
      jit_config: "eyJqaXQiOiB0cnVlfQ==".to_owned(),
      work_dir: "~/.toolu-runner/_work".to_owned(),
      data_dir: "~/.toolu-runner/runners/octo/demo".to_owned(),
      protocol_version: "v2".to_owned(),
    },
    services: ServicesSection::default(),
    cache: CacheSection::default(),
    workspace: WorkspaceSection::default(),
    shadow: ShadowSection::default(),
    network: NetworkSection::default(),
    auth,
    provider: config::provider::ProviderSection::Github,
  }
}

fn app() -> AuthSection {
  AuthSection::App {
    app_file: "/home/u/.toolu-runner/github-app.json".to_owned(),
    installation_id: 987_654,
  }
}

#[test]
fn a_token_registration_writes_no_auth_table_and_loads_as_token() {
  let dir = TempDir::new().expect("temp dir");
  let path = dir.path().join("config.toml");
  save_config(&path, &registration(AuthSection::Token)).expect("save");
  let text = std::fs::read_to_string(&path).expect("read");
  assert!(
    !text.contains("[auth]"),
    "token keeps the pre-#201 shape:\n{text}"
  );
  assert_eq!(load_config(&path).expect("load").auth, AuthSection::Token);
}

#[test]
fn an_app_registration_round_trips_its_auth_table() {
  let dir = TempDir::new().expect("temp dir");
  let path = dir.path().join("config.toml");
  let original = registration(app());
  save_config(&path, &original).expect("save");
  let text = std::fs::read_to_string(&path).expect("read");
  assert!(text.contains("[auth]\nkind = \"app\""), "{text}");
  assert!(text.contains("installation_id = 987654"), "{text}");
  assert_eq!(load_config(&path).expect("load"), original);
}

/// Replace the `[auth]` table of a saved App registration with `table`
/// (`[auth]` is the last table `save_config` writes) and load it back.
fn load_with_auth_table(
  table: &str,
) -> Result<Result<RunnerRegistrationConfig, shared::RunnerError>, Box<dyn std::error::Error>> {
  let dir = TempDir::new()?;
  let path = dir.path().join("config.toml");
  save_config(&path, &registration(app()))?;
  let text = std::fs::read_to_string(&path)?;
  let (head, _auth) = text.split_once("[auth]").ok_or("no [auth] table")?;
  std::fs::write(&path, format!("{head}{table}"))?;
  Ok(load_config(&path))
}

#[test]
fn an_explicit_token_table_loads_as_token() {
  let cfg = load_with_auth_table("[auth]\nkind = \"token\"\n")
    .expect("setup")
    .expect("loads");
  assert_eq!(cfg.auth, AuthSection::Token);
}

#[test]
fn an_app_table_without_installation_id_is_rejected() {
  let err = load_with_auth_table("[auth]\nkind = \"app\"\napp_file = \"/x/github-app.json\"\n")
    .expect("setup")
    .expect_err("installation_id is required");
  assert!(err.to_string().contains("installation_id"), "{err}");
}

#[test]
fn an_unknown_kind_or_key_is_rejected() {
  let err = load_with_auth_table("[auth]\nkind = \"oauth\"\n")
    .expect("setup")
    .expect_err("unknown kind");
  assert!(err.to_string().contains("oauth"), "{err}");
  let err = load_with_auth_table(
    "[auth]\nkind = \"app\"\napp_file = \"/x\"\ninstallation_id = 1\ninstalation = 2\n",
  )
  .expect("setup")
  .expect_err("a typo'd key must not be ignored");
  assert!(err.to_string().contains("instalation"), "{err}");
}
