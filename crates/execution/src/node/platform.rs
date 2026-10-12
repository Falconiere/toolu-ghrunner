//! Which Node.js build an execution environment can run: OS, CPU and libc.
//!
//! The host is classified from `std::env::consts` plus its dynamic loader;
//! a job container from upstream's `/etc/*release` probe
//! (actions/runner `StepHost.DetermineNodeRuntimeVersion`). Musl builds come
//! from `actions/alpine_nodejs`, which publishes x64 only.

use std::fmt;
use std::path::Path;

use shared::RunnerError;

/// Operating system a Node.js build targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeOs {
  /// Linux (glibc or musl).
  Linux,
  /// macOS.
  Darwin,
}

/// CPU architecture a Node.js build targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeArch {
  /// `x86_64`.
  X64,
  /// `aarch64`.
  Arm64,
}

/// C library a Linux Node.js build links against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Libc {
  /// GNU libc (nodejs.org builds); also used for macOS targets.
  Glibc,
  /// musl libc (Alpine; `actions/alpine_nodejs` builds).
  Musl,
}

/// The environment a Node.js runtime must start in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeTarget {
  /// Operating system.
  pub os: NodeOs,
  /// CPU architecture.
  pub arch: NodeArch,
  /// C library (always [`Libc::Glibc`] on macOS).
  pub libc: Libc,
}

impl NodeTarget {
  /// Upstream's externals directory name: `node24` or `node24_alpine`.
  pub fn externals_name(self, major: u8) -> String {
    match self.libc {
      Libc::Glibc => format!("node{major}"),
      Libc::Musl => format!("node{major}_alpine"),
    }
  }

  /// Cache directory name under `{data_dir}/node`: `24.21.0` or `24.21.0-alpine`.
  pub fn cache_name(self, version: &str) -> String {
    match self.libc {
      Libc::Glibc => version.to_owned(),
      Libc::Musl => format!("{version}-alpine"),
    }
  }
}

impl fmt::Display for NodeTarget {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let os = match self.os {
      NodeOs::Linux => "linux",
      NodeOs::Darwin => "darwin",
    };
    let arch = match self.arch {
      NodeArch::X64 => "x64",
      NodeArch::Arm64 => "arm64",
    };
    let libc = match (self.os, self.libc) {
      (NodeOs::Darwin, _) => "",
      (NodeOs::Linux, Libc::Glibc) => " (glibc)",
      (NodeOs::Linux, Libc::Musl) => " (musl)",
    };
    write!(f, "{os}-{arch}{libc}")
  }
}

/// The target of the runner host itself.
///
/// # Errors
///
/// Returns `RunnerError::NodeRuntime` for an OS/CPU pair no Node.js build
/// exists for, or for a musl host on a non-x64 CPU.
pub fn host_target() -> Result<NodeTarget, RunnerError> {
  let libc = if std::env::consts::OS == "linux" {
    detect_libc(Path::new("/"))
  } else {
    Libc::Glibc
  };
  target_for(std::env::consts::OS, std::env::consts::ARCH, libc)
}

/// Classify an `std::env::consts` OS/arch pair with a detected libc.
///
/// # Errors
///
/// Returns `RunnerError::NodeRuntime` for an unsupported pair, or musl on a
/// non-x64 CPU.
pub fn target_for(os: &str, arch: &str, libc: Libc) -> Result<NodeTarget, RunnerError> {
  let (node_os, node_arch) = match (os, arch) {
    ("linux", "x86_64") => (NodeOs::Linux, NodeArch::X64),
    ("linux", "aarch64") => (NodeOs::Linux, NodeArch::Arm64),
    ("macos", "x86_64") => (NodeOs::Darwin, NodeArch::X64),
    ("macos", "aarch64") => (NodeOs::Darwin, NodeArch::Arm64),
    _ => {
      return Err(RunnerError::NodeRuntime(format!(
        "Node.js actions are not supported on {os} {arch}"
      )));
    },
  };
  let libc = if node_os == NodeOs::Darwin {
    Libc::Glibc
  } else {
    libc
  };
  if libc == Libc::Musl && node_arch != NodeArch::X64 {
    return Err(RunnerError::NodeRuntime(format!(
      "JavaScript Actions on musl (Alpine) hosts are only supported on x64 Linux runners. \
       Detected Linux {}",
      dotnet_arch(node_arch)
    )));
  }
  Ok(NodeTarget {
    os: node_os,
    arch: node_arch,
    libc,
  })
}

/// glibc dynamic loaders, relative to a filesystem root.
const GLIBC_LOADERS: [&str; 4] = [
  "lib64/ld-linux-x86-64.so.2",
  "lib/ld-linux-aarch64.so.1",
  "lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
  "lib/aarch64-linux-gnu/ld-linux-aarch64.so.1",
];

/// Classify the C library of the filesystem at `root` by its dynamic loader:
/// any glibc loader ⇒ glibc (a glibc host with Debian's `musl` package stays
/// glibc); otherwise a `lib/ld-musl-*.so.1` ⇒ musl; otherwise glibc, leaving
/// the final word to the runtime's startup validation.
///
/// Loader entries are checked without following symlinks, so an exported
/// root's absolute links never resolve into the host.
pub fn detect_libc(root: &Path) -> Libc {
  let present = |relative: &str| std::fs::symlink_metadata(root.join(relative)).is_ok();
  if GLIBC_LOADERS.iter().any(|loader| present(loader)) {
    return Libc::Glibc;
  }
  let musl = std::fs::read_dir(root.join("lib")).is_ok_and(|entries| {
    entries.filter_map(Result::ok).any(|entry| {
      entry
        .file_name()
        .to_str()
        .is_some_and(|name| name.starts_with("ld-musl-") && name.ends_with(".so.1"))
    })
  });
  if musl { Libc::Musl } else { Libc::Glibc }
}

/// Upstream's container probe command (`StepHost.DetermineNodeRuntimeVersion`).
pub const OS_RELEASE_PROBE: &str = "cat /etc/*release | grep ^ID";

/// The target of a Linux job container on a host CPU of `arch`, from the
/// stdout of [`OS_RELEASE_PROBE`] (`None` when the probe exited non-zero).
/// Any line containing `alpine` (case-insensitively) selects musl; otherwise
/// glibc, upstream's best effort.
///
/// # Errors
///
/// Returns upstream's `NotSupportedException` text for an Alpine container on
/// a non-x64 runner.
pub fn container_target(
  probe_stdout: Option<&str>,
  arch: NodeArch,
) -> Result<NodeTarget, RunnerError> {
  let alpine = probe_stdout.is_some_and(|stdout| {
    stdout
      .lines()
      .any(|line| line.to_lowercase().contains("alpine"))
  });
  if alpine && arch != NodeArch::X64 {
    return Err(RunnerError::NodeRuntime(format!(
      "JavaScript Actions in Alpine containers are only supported on x64 Linux runners. \
       Detected Linux {}",
      dotnet_arch(arch)
    )));
  }
  Ok(NodeTarget {
    os: NodeOs::Linux,
    arch,
    libc: if alpine { Libc::Musl } else { Libc::Glibc },
  })
}

/// Upstream's `Architecture` enum spelling, as its messages print it.
fn dotnet_arch(arch: NodeArch) -> &'static str {
  match arch {
    NodeArch::X64 => "X64",
    NodeArch::Arm64 => "Arm64",
  }
}

#[cfg(test)]
#[path = "tests/platform.rs"]
mod tests;
