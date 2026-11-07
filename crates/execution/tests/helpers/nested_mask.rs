//! Build a two-level local composite whose innermost step registers a
//! dynamic multiline `::add-mask::`, so tests can prove a mask registered
//! inside a NESTED composite reaches the job-level masker that later
//! suppresses job outputs, step summaries and deployment URLs (#86, #88).

use std::path::Path;

use shared::{ActionStep, ActionStepDefinitionReference, TemplateToken};

/// The whole multiline value [`write_actions`] registers around `value`.
///
/// `SecretMasker::add_mask` registers this whole value *and* each trimmed
/// line, and the two families catch different wrong implementations, so
/// #235's AC is only met when both are asserted at a sink. The breaks are
/// LF so the span stays matchable verbatim: `step_summary` normalizes CRLF
/// to LF before masking, which would make a CR-bearing span unreachable.
/// It starts at `value` because a leading space would put `: ` in the
/// composite's YAML plain scalar, which YAML reads as a mapping.
pub fn whole_value(value: &str) -> String {
  format!("{value}-head \n{value}\n {value}-tail ")
}

/// A second multiline value [`write_actions`] registers, broken by CRLF.
///
/// Its lines are masks only if the split handled `\r\n`, which is what a real
/// action's `%0D%0A` payload produces.
pub fn crlf_line(value: &str) -> String {
  format!("{value}-cr")
}

/// Write `outer/` (which `uses: ./inner`) and `inner/` (which registers
/// [`whole_value`] as one multiline mask) into an already-created job
/// workspace.
pub fn write_actions(workspace: &Path, value: &str) -> std::io::Result<()> {
  std::fs::create_dir_all(workspace.join("outer"))?;
  std::fs::create_dir_all(workspace.join("inner"))?;
  std::fs::write(
    workspace.join("outer/action.yml"),
    "name: outer\ndescription: nested mask probe\nruns:\n  using: composite\n  steps:\n    - uses: ./inner\n",
  )?;
  // Each value travels percent-encoded on one command line, and reaches the
  // masker through the production unescape, exactly as a real action sends it.
  let encoded = whole_value(value).replace('\n', "%0A");
  let crlf = format!("{}%0D%0A{value}-lf", crlf_line(value));
  std::fs::write(
    workspace.join("inner/action.yml"),
    format!(
      "name: inner\ndescription: nested mask probe\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: printf '%s\\n' '::add-mask::{encoded}' '::add-mask::{crlf}'\n"
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
