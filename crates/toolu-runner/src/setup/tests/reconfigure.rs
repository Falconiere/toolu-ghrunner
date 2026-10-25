//! `owned_with`: every caller flag survives the re-run menu.

use super::{empty_args, owned_with};

#[test]
fn caller_flags_thread_through_reconfigure() {
  let mut caller = empty_args();
  caller.runner_group = Some("Custom".to_owned());
  caller.work_dir = Some(std::path::PathBuf::from("/tmp/caller-work"));
  caller.import = Some(std::path::PathBuf::from("/tmp/app.json"));
  caller.app_id = Some(42);
  caller.app_key = Some(std::path::PathBuf::from("/tmp/app.pem"));
  caller.token_stdin = true;
  caller.mode.no_service = true;
  caller.mode.ephemeral = true;
  caller.flow.enable_linger = true;
  caller.labels = vec!["extra".to_owned()];
  let owned = owned_with(&caller, empty_args());
  assert_eq!(owned.runner_group.as_deref(), Some("Custom"));
  assert_eq!(
    owned.work_dir,
    Some(std::path::PathBuf::from("/tmp/caller-work"))
  );
  assert_eq!(
    owned.import,
    Some(std::path::PathBuf::from("/tmp/app.json"))
  );
  assert_eq!(owned.app_id, Some(42));
  assert_eq!(
    owned.app_key,
    Some(std::path::PathBuf::from("/tmp/app.pem"))
  );
  assert!(owned.token_stdin);
  assert!(owned.mode.no_service);
  assert!(owned.mode.ephemeral);
  assert!(owned.flow.enable_linger);
  assert_eq!(owned.labels, vec!["extra".to_owned()]);
  assert!(
    !owned.no_default_labels,
    "caller labels stay additive without --no-default-labels"
  );
}
