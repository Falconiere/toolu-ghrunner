//! Tests for the `doctor` service rows: parsing the real one-line answers of
//! `systemctl --user is-active` and `loginctl show-user --property=Linger`,
//! and the row each unit state renders.

use std::path::Path;

use super::{Activity, installed_row, linger_row_for, parse_is_active, parse_linger};
use crate::doctor_report::Level;

#[test]
fn systemctl_is_active_states_map_to_running_or_not() {
  for running in ["active\n", "reloading\n", "activating\n"] {
    assert_eq!(parse_is_active(running), Some(true), "{running:?}");
  }
  for stopped in ["inactive\n", "failed\n", "deactivating\n"] {
    assert_eq!(parse_is_active(stopped), Some(false), "{stopped:?}");
  }
  assert_eq!(parse_is_active(""), None);
  assert_eq!(parse_is_active("Failed to connect to bus\n"), None);
}

#[test]
fn loginctl_linger_property_is_parsed() {
  assert_eq!(parse_linger("Linger=yes\n"), Some(true));
  assert_eq!(parse_linger("Linger=no\n"), Some(false));
  assert_eq!(parse_linger(""), None);
  assert_eq!(parse_linger("Linger=maybe\n"), None);
}

#[test]
fn unit_states_render_ok_warn_with_activation_or_query_failure() {
  let dest = Path::new("/h/.config/systemd/user/toolu-runner-o-r.service");
  let active = installed_row(dest, &Activity::Running);
  assert_eq!(active.level, Level::Ok);
  assert_eq!(
    active.detail,
    "installed and active (/h/.config/systemd/user/toolu-runner-o-r.service)"
  );

  let stopped = installed_row(
    dest,
    &Activity::Stopped("systemctl --user enable --now toolu-runner-o-r.service".to_owned()),
  );
  assert_eq!(stopped.level, Level::Warn);
  assert!(
    stopped.detail.starts_with("installed ("),
    "{}",
    stopped.detail
  );
  assert!(
    stopped.detail.ends_with("), not active"),
    "{}",
    stopped.detail
  );
  assert_eq!(
    stopped.fix.as_deref(),
    Some("systemctl --user enable --now toolu-runner-o-r.service")
  );

  let unknown = installed_row(
    dest,
    &Activity::Unknown(
      "systemctl".to_owned(),
      "Failed to connect to bus".to_owned(),
    ),
  );
  assert_eq!(unknown.level, Level::Warn);
  assert!(
    unknown
      .detail
      .ends_with("; could not query systemctl: Failed to connect to bus"),
    "{}",
    unknown.detail
  );
}

#[test]
fn linger_off_warns_with_enable_linger_for_the_user() {
  let on = linger_row_for("ci", Some(true));
  assert_eq!(on.level, Level::Ok);
  let off = linger_row_for("ci", Some(false));
  assert_eq!(off.level, Level::Warn);
  assert_eq!(off.fix.as_deref(), Some("loginctl enable-linger ci"));
  let unknown = linger_row_for("ci", None);
  assert_eq!(unknown.level, Level::Warn);
  assert!(
    unknown.detail.starts_with("could not query"),
    "{}",
    unknown.detail
  );
}
