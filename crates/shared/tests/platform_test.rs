//! Tests for `shared::platform` — the host-derived `RUNNER_OS` / `RUNNER_ARCH`
//! labels.
//!
//! Real data: the host this test compiles for. `std::env::consts::OS` / `ARCH`
//! are the same values the runner reads at job time, so the assertions below
//! exercise the real mapping rather than a stubbed target.

use shared::platform::{runner_arch, runner_os};

/// The label is derived from the host, not pinned to one target: it must equal
/// GitHub's spelling for whatever `std::env::consts::OS` this build reports.
#[test]
fn runner_os_is_the_github_spelling_for_this_host() {
  let expected = match std::env::consts::OS {
    "linux" => "Linux",
    "macos" => "macOS",
    "windows" => "Windows",
    other => other,
  };
  assert_eq!(runner_os(), expected);
}

/// A macOS host must never report `Linux` — the regression this module exists
/// to prevent (workflows branch on `runner.os == 'macOS'`).
#[test]
fn runner_os_never_reports_linux_off_linux() {
  if std::env::consts::OS == "linux" {
    assert_eq!(runner_os(), "Linux");
  } else {
    assert_ne!(runner_os(), "Linux");
  }
}

/// Only GitHub's three canonical spellings are produced on the three targets
/// the runner supports; anything else would silently mis-route `runner.os`.
#[test]
fn runner_os_is_one_of_the_canonical_values_on_supported_targets() {
  if matches!(std::env::consts::OS, "linux" | "macos" | "windows") {
    assert!(matches!(runner_os(), "Linux" | "macOS" | "Windows"));
  }
}

/// The arch mapping is unchanged by the OS work — pinned here so both host
/// labels are covered by one module.
#[test]
fn runner_arch_is_the_github_spelling_for_this_host() {
  let expected = match std::env::consts::ARCH {
    "x86_64" => "X64",
    "aarch64" => "ARM64",
    "arm" => "ARM",
    "x86" => "X86",
    other => other,
  };
  assert_eq!(runner_arch(), expected);
}
