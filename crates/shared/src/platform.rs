//! Runner OS/arch reporting in GitHub's `RUNNER_OS` / `RUNNER_ARCH` naming.
//!
//! Pure host-derived helpers shared by the broker poll/acknowledge paths and
//! the `runner.*` execution context, so every caller derives os/arch one way.

/// GitHub `RUNNER_OS` for this build target. Linux only (see non-goals).
pub fn runner_os() -> &'static str {
  "Linux"
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
