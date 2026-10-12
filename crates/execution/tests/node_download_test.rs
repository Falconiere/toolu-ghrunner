//! Opt-in network lane for #95: real runtime downloads through the
//! production acquisition path. A nodejs.org build for this host installs
//! only after it starts; the real `actions/alpine_nodejs` build host-targeted
//! commits only where it can start (a glibc host needs a musl loader for
//! that), yet always installs for a container target (where the Docker lane
//! proves it runs).

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
async fn real_alpine_runtime_is_start_checked_on_glibc_and_installs_for_containers() -> TestResult {
  assert_eq!(
    detect_libc(Path::new("/")),
    Libc::Glibc,
    "needs a glibc host"
  );
  let client = reqwest::Client::new();

  // The musl build's interpreter is `/lib/ld-musl-x86_64.so.1`. A plain glibc
  // host (CI ubuntu) lacks it, so the start check must refuse the install; a
  // glibc host with Debian's `musl` package can really run it, so the check
  // passes and the runtime commits. Either way only a runtime that started
  // is ever committed.
  let can_start = std::fs::symlink_metadata("/lib/ld-musl-x86_64.so.1").is_ok();
  let host = tempfile::tempdir()?;
  let host_targeted = ensure_node_runtime_from(
    &PRODUCTION_BASES,
    &client,
    &config(host.path()),
    24,
    MUSL_X64,
    true,
  )
  .await;
  let committed = node_runtime_dir(host.path(), "24.21.0", MUSL_X64);
  if can_start {
    let binary = host_targeted.map_err(|e| e.to_string())?;
    let output = std::process::Command::new(&binary)
      .arg("--version")
      .output()?;
    assert_eq!(String::from_utf8(output.stdout)?.trim(), "v24.21.0");
  } else {
    let message = host_targeted
      .map_err(|e| e.to_string())
      .err()
      .ok_or("a musl runtime must not pass the start check without a musl loader")?;
    assert!(
      message.contains("was not installed") && message.contains("failed to start on this host"),
      "{message}"
    );
    assert!(!committed.exists());
  }

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
