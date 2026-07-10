//! Tests for `resolve_handler` — validates that step `runs.using` values
//! map to the correct `HandlerKind` variant, including plugin override.
//!
//! Uses `ActionStep::with_ref_type` to construct real-shape step fixtures.

use shared::ActionStep;
use toolu_runner::execution::handlers::{HandlerKind, resolve_handler};
use toolu_runner::plugin::{PluginRegistry, RunnerPlugin};

/// A test plugin that names itself and always succeeds.
struct EchoPlugin(&'static str);

#[async_trait::async_trait]
impl RunnerPlugin for EchoPlugin {
  fn name(&self) -> &str {
    self.0
  }
  async fn execute_step(
    &self,
    _step: &shared::ActionStep,
    _ctx: &toolu_runner::execution::context::ExecutionContext,
    _events: &tokio::sync::mpsc::Sender<shared::RunnerEvent>,
  ) -> shared::Conclusion {
    shared::Conclusion::Success
  }
}

#[test]
fn run_step_resolves_to_script() {
  let step = ActionStep::script("s1", "echo hi", "");
  let plugins = PluginRegistry::new();
  assert_eq!(resolve_handler(&step, &plugins), HandlerKind::Script);
}

#[test]
fn node12_resolves_to_node() {
  let step = ActionStep::with_ref_type("s1", "node12");
  let plugins = PluginRegistry::new();
  assert_eq!(resolve_handler(&step, &plugins), HandlerKind::Node);
}

#[test]
fn node16_resolves_to_node() {
  let step = ActionStep::with_ref_type("s1", "node16");
  let plugins = PluginRegistry::new();
  assert_eq!(resolve_handler(&step, &plugins), HandlerKind::Node);
}

#[test]
fn node20_resolves_to_node() {
  let step = ActionStep::with_ref_type("s1", "node20");
  let plugins = PluginRegistry::new();
  assert_eq!(resolve_handler(&step, &plugins), HandlerKind::Node);
}

#[test]
fn node24_resolves_to_node() {
  let step = ActionStep::with_ref_type("s1", "node24");
  let plugins = PluginRegistry::new();
  assert_eq!(resolve_handler(&step, &plugins), HandlerKind::Node);
}

#[test]
fn script_ref_type_resolves_to_script() {
  let step = ActionStep::with_ref_type("s1", "script");
  let plugins = PluginRegistry::new();
  assert_eq!(resolve_handler(&step, &plugins), HandlerKind::Script);
}

#[test]
fn docker_resolves_to_docker() {
  let step = ActionStep::with_ref_type("s1", "docker");
  let plugins = PluginRegistry::new();
  assert_eq!(resolve_handler(&step, &plugins), HandlerKind::Docker);
}

#[test]
fn composite_resolves_to_composite() {
  let step = ActionStep::with_ref_type("s1", "composite");
  let plugins = PluginRegistry::new();
  assert_eq!(resolve_handler(&step, &plugins), HandlerKind::Composite);
}

#[test]
fn unknown_ref_type_returns_unknown() {
  let step = ActionStep::with_ref_type("s1", "telepathy");
  let plugins = PluginRegistry::new();
  assert_eq!(
    resolve_handler(&step, &plugins),
    HandlerKind::Unknown("telepathy".to_owned())
  );
}

#[test]
fn plugin_overrides_builtin_handler() {
  let step = ActionStep::with_ref_type("s1", "telepathy");
  let mut plugins = PluginRegistry::new();
  plugins.register(Box::new(EchoPlugin("telepathy")));
  assert_eq!(
    resolve_handler(&step, &plugins),
    HandlerKind::Plugin("telepathy")
  );
}
