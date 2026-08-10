//! B-004: pins the single-source runner path helpers to the underscore
//! literals (`_temp` / `_tool`) that `set_runner_context` creates at job
//! start. The convergence itself is structural — `apply_runner_paths` (node
//! stages) and `composite_env`/`composite_exec` call these helpers instead of
//! joining their own paths — so re-divergence requires changing these two
//! values; this test makes that change fail loudly.

use std::path::{Path, PathBuf};

use execution::execution::context::{runner_temp_dir, runner_tool_cache_dir};

#[test]
fn runner_temp_dir_is_data_dir_underscore_temp() {
  let data_dir = Path::new("/var/lib/toolu-runner");
  assert_eq!(
    runner_temp_dir(data_dir),
    PathBuf::from("/var/lib/toolu-runner/_temp")
  );
}

#[test]
fn runner_tool_cache_dir_is_data_dir_underscore_tool() {
  let data_dir = Path::new("/var/lib/toolu-runner");
  assert_eq!(
    runner_tool_cache_dir(data_dir),
    PathBuf::from("/var/lib/toolu-runner/_tool")
  );
}
