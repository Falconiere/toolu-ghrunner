//! Build a two-level local composite whose innermost step registers a
//! dynamic `::add-mask::`, so tests can prove a mask registered inside a
//! NESTED composite reaches the job-level masker that later suppresses job
//! outputs, step summaries and deployment URLs (#86, #88).

use std::path::Path;

use shared::{ActionStep, ActionStepDefinitionReference, TemplateToken};

/// Write `outer/` (which `uses: ./inner`) and `inner/` (which registers
/// `value` as a mask) into an already-created job workspace.
pub fn write_actions(workspace: &Path, value: &str) -> std::io::Result<()> {
  std::fs::create_dir_all(workspace.join("outer"))?;
  std::fs::create_dir_all(workspace.join("inner"))?;
  std::fs::write(
    workspace.join("outer/action.yml"),
    "name: outer\ndescription: nested mask probe\nruns:\n  using: composite\n  steps:\n    - uses: ./inner\n",
  )?;
  std::fs::write(
    workspace.join("inner/action.yml"),
    format!(
      "name: inner\ndescription: nested mask probe\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: printf '::add-mask::{value}\\n'\n"
    ),
  )
}

/// A step that runs the composite [`write_actions`] wrote.
pub fn outer_step(id: &str) -> ActionStep {
  ActionStep {
    reference: ActionStepDefinitionReference {
      ref_type: Some("repository".to_owned()),
      name: Some("./outer".to_owned()),
      repository_type: Some("Local".to_owned()),
      ..ActionStepDefinitionReference::script()
    },
    inputs: TemplateToken::default(),
    ..ActionStep::script(id, "", "")
  }
}
