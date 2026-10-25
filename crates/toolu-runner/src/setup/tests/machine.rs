//! Machine facts over a real temp home with a real registration.

use super::*;

#[test]
fn gather_lists_existing_registrations() {
  let home = std::env::temp_dir().join(format!("toolu-setup-machine-{}", std::process::id()));
  let dir = home.join("runners/octo/web");
  std::fs::create_dir_all(&dir).unwrap();
  std::fs::write(dir.join("config.toml"), "ephemeral = false\n").unwrap();
  let facts = gather(&home).unwrap();
  assert_eq!(facts.existing.len(), 1);
  assert!(facts.summary().contains("1 existing runner(s)"));
  std::fs::remove_dir_all(&home).ok();
}

#[test]
fn headless_classification_matches_platforms() {
  assert!(classify_headless(true, true, false));
  assert!(
    classify_headless(true, false, true),
    "SSH on a Mac is headless"
  );
  assert!(
    !classify_headless(false, false, true),
    "a Mac desktop is not headless"
  );
  assert!(
    classify_headless(false, false, false),
    "no display on Linux is headless"
  );
  assert!(!classify_headless(false, true, false));
}
