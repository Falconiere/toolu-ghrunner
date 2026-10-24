//! Root per-registration unit collision checks over real rendered unit files.

use std::path::Path;

use config::service_unit::{ServiceSpec, systemd_unit};

use super::check_existing_system_unit;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn unit_for(config: &Path) -> String {
  systemd_unit(&ServiceSpec {
    label: "test",
    exe: Path::new("/usr/local/bin/toolu-runner"),
    config_path: config,
    diag_dir: Path::new("/tmp/_diag"),
    env_path: "/usr/bin:/bin",
    work_dir: Path::new("/tmp"),
  })
}

#[test]
fn root_registration_reuses_its_unit_but_refuses_a_live_foreign_target() -> TestResult {
  let dir = tempfile::tempdir()?;
  let mine = dir.path().join("mine/config.toml");
  let foreign = dir.path().join("foreign/config.toml");
  for config in [&mine, &foreign] {
    std::fs::create_dir_all(config.parent().ok_or("no config parent")?)?;
    std::fs::write(config, "registration")?;
  }
  let dest = dir.path().join("toolu-runner-o-r.service");
  assert!(check_existing_system_unit(&dest, &mine).is_ok());
  std::fs::write(&dest, unit_for(&foreign))?;
  assert!(check_existing_system_unit(&dest, &mine).is_err());
  std::fs::write(&dest, unit_for(&mine))?;
  assert!(check_existing_system_unit(&dest, &mine).is_ok());
  std::fs::remove_file(foreign)?;
  std::fs::write(&dest, unit_for(&dir.path().join("foreign/config.toml")))?;
  assert!(check_existing_system_unit(&dest, &mine).is_ok());
  Ok(())
}

#[cfg(unix)]
#[test]
fn root_registration_does_not_write_through_a_symlink() -> TestResult {
  let dir = tempfile::tempdir()?;
  let dest = dir.path().join("toolu-runner-o-r.service");
  std::os::unix::fs::symlink("/dev/null", &dest)?;
  assert!(check_existing_system_unit(&dest, &dir.path().join("config.toml")).is_err());
  Ok(())
}
