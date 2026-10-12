//! Opt-in network lane for #95: real runtime downloads through the
//! production acquisition path. A nodejs.org build for this host installs
//! only after it starts; the real `actions/alpine_nodejs` build cannot start
//! on a glibc host, so it is never committed when host-targeted, yet installs
//! for a container target (where the Docker lane proves it runs).

use std::error::Error;
use std::path::Path;

use execution::node::platform::{Libc, NodeArch, NodeOs, NodeTarget, detect_libc, host_target};
use execution::node::runtime::{
  PRODUCTION_BASES, ensure_node_runtime, ensure_node_runtime_from, node_binary_path,
  node_runtime_dir,
};
use shared::RunnerConfig;

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

const MUSL_X64: NodeTarget = NodeTarget {
  os: NodeOs::Linux,
  arch: NodeArch::X64,
  libc: Libc::Musl,
};

fn config(data: &Path) -> RunnerConfig {
  RunnerConfig {
    data_dir: data.to_path_buf(),
    ..RunnerConfig::default()
  }
}

#[tokio::test]
#[ignore = "downloads Node.js 24.21.0 from nodejs.org"]
async fn real_nodejs_org_runtime_installs_validated_for_this_host() -> TestResult {
  let data = tempfile::tempdir()?;
  let target = host_target()?;
  let binary =
    ensure_node_runtime(&reqwest::Client::new(), &config(data.path()), 24, target).await?;
  assert_eq!(
    binary,
    node_binary_path(&node_runtime_dir(data.path(), "24.21.0", target))
  );
  let output = std::process::Command::new(&binary)
    .arg("--version")
    .output()?;
  assert_eq!(String::from_utf8(output.stdout)?.trim(), "v24.21.0");
  Ok(())
}

#[tokio::test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[ignore = "downloads Node.js 24.21.0 from github.com/actions/alpine_nodejs"]
async fn real_alpine_runtime_is_rejected_on_glibc_but_installs_for_containers() -> TestResult {
  assert_eq!(
    detect_libc(Path::new("/")),
    Libc::Glibc,
    "needs a glibc host"
  );
  let client = reqwest::Client::new();

  let host = tempfile::tempdir()?;
  let rejected = ensure_node_runtime_from(
    &PRODUCTION_BASES,
    &client,
    &config(host.path()),
    24,
    MUSL_X64,
    true,
  )
  .await;
  let message = rejected
    .map_err(|e| e.to_string())
    .err()
    .ok_or("must be rejected")?;
  assert!(
    message.contains("was not installed") && message.contains("failed to start on this host"),
    "{message}"
  );
  assert!(!node_runtime_dir(host.path(), "24.21.0", MUSL_X64).exists());

  let container = tempfile::tempdir()?;
  let binary = ensure_node_runtime_from(
    &PRODUCTION_BASES,
    &client,
    &config(container.path()),
    24,
    MUSL_X64,
    false,
  )
  .await?;
  assert_eq!(
    binary,
    container.path().join("node/24.21.0-alpine/bin/node")
  );
  let elf = std::fs::read(&binary)?;
  assert_eq!(elf.get(..4), Some(&b"\x7fELF"[..]), "a real executable");
  Ok(())
}
