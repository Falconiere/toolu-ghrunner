//! Tests for `shared::platform` — the host-derived `RUNNER_OS` / `RUNNER_ARCH`
//! labels.
//!
//! Real data: the host this test compiles for. `std::env::consts::OS` / `ARCH`
//! are the same values the runner reads at job time, so the assertions below
//! exercise the real mapping rather than a stubbed target.

use shared::platform::{runner_arch, runner_os};

/// The label is derived from the host, not pinned to one target.
///
/// The expected value is a literal chosen by `cfg!(target_os = …)` — a
/// compile-time source independent of the runtime `std::env::consts::OS` the
/// production mapping reads. Re-deriving it from the same `match` would make
/// the test agree with any typo the mapping contains.
#[test]
fn runner_os_is_the_github_spelling_for_this_host() {
  if cfg!(target_os = "linux") {
    assert_eq!(runner_os(), "Linux");
  } else if cfg!(target_os = "macos") {
    assert_eq!(runner_os(), "macOS");
  } else if cfg!(target_os = "windows") {
    assert_eq!(runner_os(), "Windows");
  } else {
    // Any other target reports verbatim; nothing to pin to a literal.
    assert_eq!(runner_os(), std::env::consts::OS);
  }
}

/// `Linux` is reported on Linux and nowhere else — the regression this module
/// exists to prevent (workflows branch on `runner.os == 'macOS'`). Both
/// directions are asserted, so the label cannot go back to a constant.
#[test]
fn runner_os_reports_linux_only_on_linux() {
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
/// labels are covered by one module. Same technique as the OS test: the
/// expected value is a literal selected by the compile-time `cfg!`, not
/// re-derived from the runtime constant the production mapping reads.
#[test]
fn runner_arch_is_the_github_spelling_for_this_host() {
  if cfg!(target_arch = "x86_64") {
    assert_eq!(runner_arch(), "X64");
  } else if cfg!(target_arch = "aarch64") {
    assert_eq!(runner_arch(), "ARM64");
  } else if cfg!(target_arch = "arm") {
    assert_eq!(runner_arch(), "ARM");
  } else if cfg!(target_arch = "x86") {
    assert_eq!(runner_arch(), "X86");
  } else {
    assert_eq!(runner_arch(), std::env::consts::ARCH);
  }
}
