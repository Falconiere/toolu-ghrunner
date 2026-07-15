//! Unit tests for `config::service_unit::service_action_summary` — the pure
//! status-line builder behind `install-service`'s output.
//!
//! Real data, no mocks: the label / path pair mirror a real per-repo
//! registration (`io.toolu.runner.octo.demo` and its unit under a user home),
//! and the asserted strings are the exact lines the `install-service` CLI
//! prints for each outcome. This gives the bin-internal `install_service_core`
//! decision logic — now pure in this crate — direct unit coverage.

use std::path::Path;

use config::service_unit::{ServiceAction, service_action_summary};

/// The launchd `Label` / systemd `Description` suffix for the octo/demo repo.
const LABEL: &str = "io.toolu.runner.octo.demo";

/// A representative unit-file path (a systemd user unit under a home dir).
const UNIT_PATH: &str = "/home/ci/.config/systemd/user/toolu-runner-octo-demo.service";

#[test]
fn installed_and_activated_reports_install_and_path() {
  let summary = service_action_summary(
    &ServiceAction::Installed { activated: true },
    LABEL,
    Path::new(UNIT_PATH),
  );
  assert_eq!(
    summary,
    "installed io.toolu.runner.octo.demo and activated it at \
     /home/ci/.config/systemd/user/toolu-runner-octo-demo.service"
  );
}

#[test]
fn installed_without_activation_reports_wrote_path() {
  let summary = service_action_summary(
    &ServiceAction::Installed { activated: false },
    LABEL,
    Path::new(UNIT_PATH),
  );
  assert_eq!(
    summary,
    "wrote /home/ci/.config/systemd/user/toolu-runner-octo-demo.service"
  );
}

#[test]
fn removed_reports_label_and_path() {
  let summary = service_action_summary(&ServiceAction::Removed, LABEL, Path::new(UNIT_PATH));
  assert_eq!(
    summary,
    "removed io.toolu.runner.octo.demo \
     (/home/ci/.config/systemd/user/toolu-runner-octo-demo.service)"
  );
}

#[test]
fn nothing_to_remove_reports_nothing_to_do() {
  let summary =
    service_action_summary(&ServiceAction::NothingToRemove, LABEL, Path::new(UNIT_PATH));
  assert_eq!(
    summary,
    "no service installed at \
     /home/ci/.config/systemd/user/toolu-runner-octo-demo.service — nothing to do"
  );
}

#[test]
fn printed_has_no_status_line() {
  let summary = service_action_summary(&ServiceAction::Printed, LABEL, Path::new(UNIT_PATH));
  assert_eq!(
    summary, "",
    "print mode emits the unit text, not a status line"
  );
}
