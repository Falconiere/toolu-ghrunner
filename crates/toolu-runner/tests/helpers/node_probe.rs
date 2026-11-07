//! A real Node action that dumps its `ACTIONS_*` environment to a file, for
//! jobs driven through `run_job` where only a Node/container step receives the
//! service variables (a `run:` step no longer does).

use std::error::Error;
use std::path::Path;

use shared::ActionStep;

use crate::gh_compat_node_action_file_commands::{seed_node, system_node};

/// Install the probe at `<workspace_root>/<job_id>/node-probe` (the
/// `WorkspaceMode::Fresh` layout) and return its `uses: ./node-probe` step.
///
/// The action writes `KEY=VALUE` lines for every `ACTIONS_*` variable to `dump`
/// (via rename, so a reader never sees a partial file), then — when `sentinel`
/// is given — blocks until that file exists. `None` means no system `node`.
pub fn install(
  workspace_root: &Path,
  data_dir: &Path,
  job_id: &str,
  dump: &Path,
  sentinel: Option<&Path>,
) -> Result<Option<ActionStep>, Box<dyn Error>> {
  let Some(node) = system_node() else {
    eprintln!("SKIP: no system `node` on PATH; this test needs a real node runtime");
    return Ok(None);
  };
  seed_node(data_dir, &node)?;
  let dir = workspace_root
    .join(shared::paths::sanitize_job_id(job_id))
    .join("node-probe");
  std::fs::create_dir_all(&dir)?;
  let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/service_env_action");
  std::fs::copy(fixtures.join("action.yml"), dir.join("action.yml"))?;
  let main = std::fs::read_to_string(fixtures.join("main.js"))?
    .replace("__DUMP__", &dump.to_string_lossy())
    .replace(
      "__SENTINEL__",
      &sentinel
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default(),
    );
  std::fs::write(dir.join("main.js"), main)?;
  let mut step = ActionStep::with_ref_type("node-probe", "repository");
  step.reference.name = Some("./node-probe".to_owned());
  Ok(Some(step))
}
