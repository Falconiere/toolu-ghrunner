//! Tests for `platform`: this host's real classification, the supported
//! OS/CPU table and its errors, loader-based libc detection over roots laid
//! out like the captured images, and the container probe over real probe
//! output.

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// `sh -c "cat /etc/*release | grep ^ID"` stdout, captured 2026-10-12 from
/// `alpine@sha256:5291449c3df73caf6ed85e649dec1b9e818b39a5d8c871e97afc13e9cd5e8fa8`.
const ALPINE_PROBE: &str = "ID=alpine\n";
/// Same probe, `ubuntu@sha256:534baea6a22c03a63003dbc8dbe78fe34bc0d7e595d9a9dc9834884ff530eb55`.
const UBUNTU_PROBE: &str = "ID=ubuntu\nID_LIKE=debian\n";

fn touch(root: &Path, relative: &str) -> TestResult {
  let path = root.join(relative);
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent)?;
  }
  std::fs::write(path, b"\x7fELF")?;
  Ok(())
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn this_linux_x64_glibc_host_is_classified_from_its_real_loader() -> TestResult {
  assert_eq!(detect_libc(Path::new("/")), Libc::Glibc);
  assert_eq!(
    host_target()?,
    NodeTarget {
      os: NodeOs::Linux,
      arch: NodeArch::X64,
      libc: Libc::Glibc,
    }
  );
  Ok(())
}

#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn this_macos_arm64_host_targets_darwin_arm64() -> TestResult {
  assert_eq!(
    host_target()?,
    NodeTarget {
      os: NodeOs::Darwin,
      arch: NodeArch::Arm64,
      libc: Libc::Glibc,
    }
  );
  Ok(())
}

#[test]
fn unsupported_os_or_cpu_fails_clearly() {
  for (os, arch) in [
    ("linux", "arm"),
    ("freebsd", "x86_64"),
    ("windows", "x86_64"),
  ] {
    let error = target_for(os, arch, Libc::Glibc).map(|_| ());
    assert_eq!(
      error.map_err(|e| e.to_string()),
      Err(format!(
        "node runtime error: Node.js actions are not supported on {os} {arch}"
      )),
    );
  }
}

#[test]
fn musl_host_is_x64_only() -> TestResult {
  assert_eq!(target_for("linux", "x86_64", Libc::Musl)?.libc, Libc::Musl);
  let error = target_for("linux", "aarch64", Libc::Musl).map(|_| ());
  assert_eq!(
    error.map_err(|e| e.to_string()),
    Err(
      "node runtime error: JavaScript Actions on musl (Alpine) hosts are only supported on x64 \
       Linux runners. Detected Linux Arm64"
        .to_owned()
    ),
  );
  // macOS has no musl build; its libc is irrelevant.
  assert_eq!(
    target_for("macos", "aarch64", Libc::Musl)?.libc,
    Libc::Glibc
  );
  Ok(())
}

#[test]
fn alpine_layout_root_is_musl() -> TestResult {
  let root = tempfile::tempdir()?;
  touch(root.path(), "lib/ld-musl-x86_64.so.1")?;
  assert_eq!(detect_libc(root.path()), Libc::Musl);
  Ok(())
}

#[test]
fn glibc_loader_wins_over_an_installed_musl_loader() -> TestResult {
  // Debian's `musl` package adds `/lib/ld-musl-x86_64.so.1` to a glibc host.
  let root = tempfile::tempdir()?;
  touch(root.path(), "lib/x86_64-linux-gnu/ld-linux-x86-64.so.2")?;
  touch(root.path(), "lib/ld-musl-x86_64.so.1")?;
  assert_eq!(detect_libc(root.path()), Libc::Glibc);
  Ok(())
}

#[test]
#[cfg(unix)]
fn dangling_absolute_loader_symlink_still_counts_as_glibc() -> TestResult {
  // Ubuntu ships `/lib64/ld-linux-x86-64.so.2` as an absolute symlink; inside
  // an exported root it must not be resolved against the host.
  let root = tempfile::tempdir()?;
  std::fs::create_dir_all(root.path().join("lib64"))?;
  std::os::unix::fs::symlink(
    "/nonexistent/x86_64-linux-gnu/ld-linux-x86-64.so.2",
    root.path().join("lib64/ld-linux-x86-64.so.2"),
  )?;
  touch(root.path(), "lib/ld-musl-x86_64.so.1")?;
  assert_eq!(detect_libc(root.path()), Libc::Glibc);
  Ok(())
}

#[test]
fn root_without_any_loader_defaults_to_glibc() -> TestResult {
  let root = tempfile::tempdir()?;
  touch(root.path(), "lib/libc.so")?;
  assert_eq!(detect_libc(root.path()), Libc::Glibc);
  Ok(())
}

#[test]
fn container_probe_selects_alpine_like_upstream() -> TestResult {
  assert_eq!(
    container_target(Some(ALPINE_PROBE), NodeArch::X64)?.libc,
    Libc::Musl
  );
  assert_eq!(
    container_target(Some(UBUNTU_PROBE), NodeArch::X64)?.libc,
    Libc::Glibc
  );
  assert_eq!(
    container_target(Some("ID=\"Alpine\"\n"), NodeArch::X64)?.libc,
    Libc::Musl
  );
  // A probe that exited non-zero (no `sh`, no release file) is glibc.
  assert_eq!(container_target(None, NodeArch::Arm64)?.libc, Libc::Glibc);
  assert_eq!(
    container_target(Some(UBUNTU_PROBE), NodeArch::Arm64)?.arch,
    NodeArch::Arm64
  );
  Ok(())
}

#[test]
fn alpine_container_on_arm64_fails_with_upstream_text() {
  let error = container_target(Some(ALPINE_PROBE), NodeArch::Arm64).map(|_| ());
  assert_eq!(
    error.map_err(|e| e.to_string()),
    Err(
      "node runtime error: JavaScript Actions in Alpine containers are only supported on x64 \
       Linux runners. Detected Linux Arm64"
        .to_owned()
    ),
  );
}

#[test]
fn target_names_follow_upstream_externals_and_cache_layout() {
  let glibc = NodeTarget {
    os: NodeOs::Linux,
    arch: NodeArch::X64,
    libc: Libc::Glibc,
  };
  let musl = NodeTarget {
    libc: Libc::Musl,
    ..glibc
  };
  assert_eq!(glibc.externals_name(20), "node20");
  assert_eq!(musl.externals_name(24), "node24_alpine");
  assert_eq!(glibc.cache_name("24.21.0"), "24.21.0");
  assert_eq!(musl.cache_name("24.21.0"), "24.21.0-alpine");
  assert_eq!(musl.to_string(), "linux-x64 (musl)");
  let darwin = NodeTarget {
    os: NodeOs::Darwin,
    arch: NodeArch::Arm64,
    libc: Libc::Glibc,
  };
  assert_eq!(darwin.to_string(), "darwin-arm64");
}
