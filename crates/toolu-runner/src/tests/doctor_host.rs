//! Tests for the `doctor` install/host rows: release comparison against the
//! captured real `releases/latest` body, disk thresholds, the nearest existing
//! ancestor for the disk probe, and the uid row.

use std::cmp::Ordering;

use super::{
  INSTALL_ONE_LINER, compare_release, disk_level, nearest_existing, uid_row, update_row,
};
use crate::doctor_report::Level;

const GIB: u64 = 1024 * 1024 * 1024;

/// `tag_name` of the captured real `GET …/releases/latest` body.
fn fixture_tag() -> Result<String, Box<dyn std::error::Error>> {
  let body: serde_json::Value = serde_json::from_str(include_str!(
    "../../tests/fixtures/doctor/releases_latest.json"
  ))?;
  Ok(
    body
      .get("tag_name")
      .and_then(serde_json::Value::as_str)
      .ok_or("fixture has no tag_name")?
      .to_owned(),
  )
}

#[test]
fn compare_release_orders_numeric_versions_against_the_real_tag()
-> Result<(), Box<dyn std::error::Error>> {
  let tag = fixture_tag()?;
  assert_eq!(tag, "v0.11.1");
  assert_eq!(compare_release("0.11.1", &tag), Some(Ordering::Equal));
  assert_eq!(compare_release("0.12.0", &tag), Some(Ordering::Greater));
  assert_eq!(compare_release("0.11.10", &tag), Some(Ordering::Greater));
  assert_eq!(compare_release("0.10.0", &tag), Some(Ordering::Less));
  assert_eq!(compare_release("0.11.1", "v1.x"), None);
  assert_eq!(compare_release("0.11.1-rc.1", &tag), None);
  assert_eq!(compare_release("0.11.1", "0.11.1"), Some(Ordering::Equal));
  Ok(())
}

#[test]
fn an_older_build_warns_with_the_install_one_liner() -> Result<(), Box<dyn std::error::Error>> {
  let tag = fixture_tag()?;
  let row = update_row("0.10.0", &tag);
  assert_eq!(row.level, Level::Warn);
  assert_eq!(row.detail, "v0.11.1 is available (installed 0.10.0)");
  assert_eq!(row.fix.as_deref(), Some(INSTALL_ONE_LINER));
  assert_eq!(
    INSTALL_ONE_LINER,
    "curl -fsSL https://get.toolu.sh/pkg/ghrunner/install | bash"
  );

  let equal = update_row("0.11.1", &tag);
  assert_eq!(
    (equal.level, equal.detail.as_str()),
    (Level::Ok, "up to date (latest v0.11.1)")
  );
  let ahead = update_row("0.12.0", &tag);
  assert_eq!(
    (ahead.level, ahead.detail.as_str()),
    (Level::Ok, "newer than the latest release (v0.11.1)")
  );
  let odd = update_row("0.11.1", "nightly");
  assert_eq!(odd.level, Level::Skip);
  assert!(odd.fix.is_none());
  Ok(())
}

#[test]
fn disk_thresholds_fail_below_1_gib_and_warn_below_5_gib() {
  assert_eq!(disk_level(0), Level::Fail);
  assert_eq!(disk_level(GIB - 1), Level::Fail);
  assert_eq!(disk_level(GIB), Level::Warn);
  assert_eq!(disk_level(5 * GIB - 1), Level::Warn);
  assert_eq!(disk_level(5 * GIB), Level::Ok);
}

#[test]
fn nearest_existing_walks_up_to_a_real_directory() -> Result<(), Box<dyn std::error::Error>> {
  let home = tempfile::tempdir()?;
  assert_eq!(
    nearest_existing(&home.path().join("a").join("b")),
    home.path()
  );
  assert_eq!(nearest_existing(home.path()), home.path());
  Ok(())
}

#[test]
fn uid_row_warns_for_root_and_unknown_uids() {
  let root = uid_row("0");
  assert_eq!(root.level, Level::Warn);
  assert!(root.detail.contains("root"), "{}", root.detail);
  assert!(root.fix.is_some());
  let user = uid_row("501");
  assert_eq!((user.level, user.detail.as_str()), (Level::Ok, "uid 501"));
  let unknown = uid_row("x");
  assert_eq!(unknown.level, Level::Warn);
  assert!(
    unknown.detail.starts_with("could not determine"),
    "{}",
    unknown.detail
  );
}
