//! B-004: pins the single-source runner path helpers to the underscore
//! literals (`_temp` / `_tool`) that `set_runner_context` creates at job
//! start. The convergence itself is structural — `apply_runner_paths` (node
//! stages) and `composite_env`/`composite_exec` call these helpers instead of
//! joining their own paths — so re-divergence requires changing these two
//! values; this test makes that change fail loudly.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use execution::Runner;
use execution::execution::context::{runner_temp_dir, runner_tool_cache_dir};
use shared::{
  ActionStep, AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker,
};
use tokio_util::sync::CancellationToken;

const CAPTURE: &str = include_str!("incoming_contexts_matrix_0.json");

/// Fails the step unless every leftover is gone and the tool cache is intact.
const LEFTOVERS_GONE: &str = r#"
file_commands="$(dirname "$GITHUB_OUTPUT")"
for root in "$RUNNER_TEMP" "$file_commands"; do
  test ! -e "$root/leftover"
  test ! -e "$root/nested"
done
test -f "$RUNNER_TOOL_CACHE/sentinel"
"#;

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

/// A crashed job's files are gone before the first step runs, and `_tool` is
/// kept, whichever step kind or file-command root they were left in.
#[tokio::test]
async fn crashed_job_leftovers_are_gone_before_first_step_and_tool_is_kept()
-> Result<(), Box<dyn Error>> {
  let dir = tempfile::tempdir()?;
  let data_dir = dir.path().join("data");
  let tool_cache = runner_tool_cache_dir(&data_dir);
  for root in [runner_temp_dir(&data_dir), data_dir.join("tmp")] {
    std::fs::create_dir_all(root.join("nested/deep"))?;
    std::fs::write(root.join("leftover"), "from a crashed job")?;
    std::fs::write(root.join("nested/deep/file"), "from a crashed job")?;
  }
  std::fs::create_dir_all(&tool_cache)?;
  std::fs::write(tool_cache.join("sentinel"), "installed tool")?;

  let mut msg: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  msg.steps = vec![ActionStep::script("probe", LEFTOVERS_GONE, "")];
  let config = RunnerConfig {
    data_dir,
    workspace_root: dir.path().join("work"),
    workspace: shared::WorkspaceConfig {
      mode: shared::WorkspaceMode::Fresh,
      stable_gc_hours: 0,
    },
    ..RunnerConfig::default()
  };
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut events = runner.execute_job(msg, CancellationToken::new());

  let mut conclusion = None;
  while let Some(event) = events.recv().await {
    if let RunnerEvent::JobCompleted { conclusion: c, .. } = event {
      conclusion = Some(c);
    }
  }
  assert_eq!(conclusion, Some(Conclusion::Success));
  assert_eq!(
    std::fs::read_to_string(tool_cache.join("sentinel"))?,
    "installed tool"
  );
  Ok(())
}
