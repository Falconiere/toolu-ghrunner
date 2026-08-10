//! Runner OS/arch reporting in GitHub's `RUNNER_OS` / `RUNNER_ARCH` naming.
//!
//! Pure host-derived helpers shared by the broker poll/acknowledge paths and
//! the `runner.*` execution context, so every caller derives os/arch one way.

/// Host OS mapped to GitHub's `RUNNER_OS` naming.
///
/// GitHub spells exactly three values — `Linux`, `macOS`, `Windows` — and
/// workflows branch on them (`if: runner.os == 'macOS'`), so the mapping is
/// from the host rather than a build-time constant. An unrecognized target is
/// warn-logged and reported verbatim, matching [`runner_arch`].
pub fn runner_os() -> &'static str {
  match std::env::consts::OS {
    "linux" => "Linux",
    "macos" => "macOS",
    "windows" => "Windows",
    other => {
      tracing::warn!(
        os = other,
        "host OS is not a canonical GitHub RUNNER_OS value; reporting it verbatim"
      );
      other
    },
  }
}

/// Host CPU arch mapped to GitHub's `RUNNER_ARCH` naming.
pub fn runner_arch() -> &'static str {
  match std::env::consts::ARCH {
    "x86_64" => "X64",
    "aarch64" => "ARM64",
    "arm" => "ARM",
    "x86" => "X86",
    other => {
      tracing::warn!(
        arch = other,
        "host arch is not a canonical GitHub RUNNER_ARCH value; reporting it verbatim"
      );
      other
    },
  }
}
