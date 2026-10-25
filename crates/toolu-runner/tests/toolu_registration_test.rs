//! #207 AC-7, AC-8: `remove`, `status` and `doctor` on a toolu.sh machine
//! registration (`[provider] kind = "toolu"`, #206 layout).
//!
//! The real binary runs against `toolu_fixtures::Peer` (toolu.sh's
//! `/runner/host` routes; the 401 is the live `dev-console.toolu.sh`
//! capture) with a real temp home seeded by `write_machine`.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

use crate::supervisor_fixtures::TestResult;
use crate::toolu_fixtures::{
  Faults, HOST_ID, HOST_TOKEN, HostFault, ORG, Peer, WORKSPACE, write_machine,
};

/// Run the binary with `args` in `home` (offline env, no stored login).
fn run(home: &Path, args: &[&str]) -> TestResult<Output> {
  Ok(
    Command::new(env!("CARGO_BIN_EXE_toolu-runner"))
      .args(args)
      .env("HOME", home)
      .env("TOOLU_RUNNER_HOME", home)
      .env("TOOLU_RUNNER_NO_KEYRING", "1")
      .env("TOOLU_RUNNER_RELEASES_URL", "http://127.0.0.1:9/releases")
      .env_remove("TOOLU_RUNNER_TOKEN")
      .output()?,
  )
}

fn text(output: &Output) -> String {
  format!(
    "{}{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  )
}

/// AC-7: `remove` sends `DELETE /runner/host` with the host token, then
/// deletes the config and the host token.
#[tokio::test]
async fn remove_revokes_on_toolu_before_deleting_local_state() -> TestResult<()> {
  let peer = Peer::start(&[], 600_000, Faults::default(), "true").await?;
  let home = TempDir::new()?;
  let config = write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let creds = config.with_file_name("credentials.json");
  let out = run(
    home.path(),
    &["remove", "--config", &config.to_string_lossy()],
  )?;
  let printed = text(&out);
  assert!(out.status.success(), "{printed}");
  let deletes = peer.seen_ending("/runner/host");
  assert_eq!(deletes.len(), 1, "{:?}", peer.paths());
  let delete = deletes.first().ok_or("no delete")?;
  assert_eq!(delete.method, "DELETE");
  assert_eq!(
    delete.authorization.as_deref(),
    Some(format!("Bearer {HOST_TOKEN}").as_str())
  );
  assert!(!config.exists() && !creds.exists(), "{printed}");
  assert!(printed.contains("revoked on toolu.sh"), "{printed}");
  assert!(!printed.contains(HOST_TOKEN), "{printed}");
  Ok(())
}

/// AC-7 failure: a 503 on the revoke keeps config and host token for a retry.
#[tokio::test]
async fn a_failed_revoke_keeps_local_state() -> TestResult<()> {
  let faults = Faults {
    host_fault: Some(HostFault::Delete503),
    ..Faults::default()
  };
  let peer = Peer::start(&[], 600_000, faults, "true").await?;
  let home = TempDir::new()?;
  let config = write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let out = run(
    home.path(),
    &["remove", "--config", &config.to_string_lossy()],
  )?;
  let printed = text(&out);
  assert!(!out.status.success(), "{printed}");
  assert!(config.exists(), "{printed}");
  assert!(config.with_file_name("credentials.json").exists());
  assert!(printed.contains("Nothing local was deleted"), "{printed}");
  Ok(())
}

/// A machine already revoked in the console (live 401) still removes.
#[tokio::test]
async fn an_already_revoked_machine_removes_locally() -> TestResult<()> {
  let faults = Faults {
    host_fault: Some(HostFault::Revoked),
    ..Faults::default()
  };
  let peer = Peer::start(&[], 600_000, faults, "true").await?;
  let home = TempDir::new()?;
  let config = write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let out = run(
    home.path(),
    &["remove", "--config", &config.to_string_lossy()],
  )?;
  let printed = text(&out);
  assert!(out.status.success(), "{printed}");
  assert!(!config.exists(), "{printed}");
  assert!(printed.contains("already revoked"), "{printed}");
  Ok(())
}

/// AC-8: `status` shows the toolu.sh source, org, host id and labels, and no
/// GitHub runner / JIT / login lines. No network.
#[tokio::test]
async fn status_prints_the_toolu_machine() -> TestResult<()> {
  let home = TempDir::new()?;
  let config = write_machine(home.path(), "http://127.0.0.1:9", HOST_TOKEN, "")?;
  let out = run(
    home.path(),
    &["status", "--config", &config.to_string_lossy()],
  )?;
  let printed = text(&out);
  assert!(out.status.success(), "{printed}");
  for want in [
    "source:    toolu.sh (http://127.0.0.1:9)",
    ORG,
    HOST_ID,
    "host token",
  ] {
    assert!(printed.contains(want), "missing {want}: {printed}");
  }
  for unwanted in ["jit_cfg", "login:", "runner_id", HOST_TOKEN] {
    assert!(!printed.contains(unwanted), "has {unwanted}: {printed}");
  }
  Ok(())
}

/// AC-8: `doctor` shows a `toolu` row (no `jit` FAIL) and an OK `host` row
/// naming the machine and workspace from `GET /runner/host`; no GitHub
/// network section for the machine.
#[tokio::test]
async fn doctor_reads_the_machine_from_toolu() -> TestResult<()> {
  let peer = Peer::start(&[], 600_000, Faults::default(), "true").await?;
  let home = TempDir::new()?;
  let config = write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let out = run(
    home.path(),
    &["doctor", "--config", &config.to_string_lossy()],
  )?;
  let printed = text(&out);
  assert!(
    printed.contains(&format!("host {HOST_ID} · workspace {WORKSPACE} · enabled")),
    "{printed}"
  );
  assert!(
    printed.contains(&format!("toolu.sh org {ORG}")),
    "{printed}"
  );
  assert!(!printed.contains("jit"), "{printed}");
  assert!(!printed.contains("network ("), "{printed}");
  assert!(!printed.contains(HOST_TOKEN), "{printed}");
  assert_eq!(peer.machines(), 0, "doctor creates nothing");
  let reads = peer.seen_ending("/runner/host");
  assert_eq!(reads.len(), 1, "{:?}", peer.paths());
  Ok(())
}

/// AC-8: a revoked machine (live 401) is a FAIL `host` row with the fix;
/// `--offline` sends nothing and skips the row.
#[tokio::test]
async fn doctor_fails_a_revoked_machine_and_skips_offline() -> TestResult<()> {
  let faults = Faults {
    host_fault: Some(HostFault::Revoked),
    ..Faults::default()
  };
  let peer = Peer::start(&[], 600_000, faults, "true").await?;
  let home = TempDir::new()?;
  let config = write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let path = config.to_string_lossy().into_owned();
  let out = run(home.path(), &["doctor", "--config", &path])?;
  let printed = text(&out);
  assert_eq!(out.status.code(), Some(1), "{printed}");
  assert!(printed.contains("the machine was revoked"), "{printed}");
  assert!(
    printed.contains("toolu-runner setup --source toolu"),
    "{printed}"
  );
  let before = peer.seen().len();
  let offline = run(home.path(), &["doctor", "--offline", "--config", &path])?;
  let printed = text(&offline);
  assert_eq!(peer.seen().len(), before, "offline sent a request");
  assert!(printed.contains("toolu.sh ("), "{printed}");
  Ok(())
}

/// `--skip-unregister` removes local state without asking toolu.sh, and says
/// the machine still exists there.
#[tokio::test]
async fn skip_unregister_leaves_the_machine_on_toolu() -> TestResult<()> {
  let peer = Peer::start(&[], 600_000, Faults::default(), "true").await?;
  let home = TempDir::new()?;
  let config = write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  let path = config.to_string_lossy().into_owned();
  let out = run(
    home.path(),
    &["remove", "--config", &path, "--skip-unregister"],
  )?;
  let printed = text(&out);
  assert!(out.status.success(), "{printed}");
  assert!(peer.seen().is_empty(), "{:?}", peer.paths());
  assert!(!config.exists(), "{printed}");
  assert!(
    printed.contains("still a machine on toolu.sh — revoke it in the console"),
    "{printed}"
  );
  Ok(())
}

/// Without its host token the machine cannot be revoked: nothing local goes.
#[tokio::test]
async fn a_missing_host_token_keeps_local_state() -> TestResult<()> {
  let peer = Peer::start(&[], 600_000, Faults::default(), "true").await?;
  let home = TempDir::new()?;
  let config = write_machine(home.path(), &peer.server.uri(), HOST_TOKEN, "")?;
  std::fs::remove_file(config.with_file_name("credentials.json"))?;
  let out = run(
    home.path(),
    &["remove", "--config", &config.to_string_lossy()],
  )?;
  let printed = text(&out);
  assert!(!out.status.success(), "{printed}");
  assert!(config.exists(), "{printed}");
  assert!(printed.contains("Nothing local was deleted"), "{printed}");
  assert!(peer.seen().is_empty(), "{:?}", peer.paths());
  Ok(())
}
