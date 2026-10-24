//! #201: `[auth] app_file` is persisted as an absolute path, or `register`
//! fails naming the file — never a relative path `run` would resolve
//! against another directory.

use std::path::Path;

use shared::RunnerError;

use super::absolute_app_file;

#[test]
fn an_absolute_app_file_is_kept_as_is() -> Result<(), RunnerError> {
  let path = Path::new("/home/runner/.toolu-runner/github-app.json");
  assert_eq!(absolute_app_file(path)?, path);
  Ok(())
}

#[test]
fn a_relative_app_file_is_resolved_against_the_current_directory()
-> Result<(), Box<dyn std::error::Error>> {
  let resolved = absolute_app_file(Path::new("home/github-app.json"))?;
  assert_eq!(
    resolved,
    std::env::current_dir()?.join("home/github-app.json")
  );
  Ok(())
}

/// `std::path::absolute` refuses an empty path, the one input that fails
/// without making the current directory unreadable.
#[test]
fn a_path_that_cannot_be_made_absolute_is_a_config_error_naming_it() {
  let result = absolute_app_file(Path::new(""));
  assert!(
    matches!(
      &result,
      Err(RunnerError::Config(msg))
        if msg.starts_with("cannot record the GitHub App file '' as an absolute path:")
    ),
    "{result:?}"
  );
}
