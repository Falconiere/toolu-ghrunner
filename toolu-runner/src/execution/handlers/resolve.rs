//! Step handler resolution logic.

use shared::ActionStep;

use crate::plugin::PluginRegistry;

/// The resolved handler type for a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerKind<'a> {
  /// A registered plugin handles this step.
  Plugin(&'a str),
  /// Built-in script handler (run: steps).
  Script,
  /// Built-in Node.js handler (node12, node16, node20, node24).
  Node,
  /// Built-in Docker handler (docker://).
  Docker,
  /// Built-in composite handler.
  Composite,
  /// Unknown handler type.
  Unknown(String),
}

/// Resolve which handler should execute a step.
///
/// Plugin registry is checked FIRST. If a registered plugin's name matches
/// the step's `runs.using` value, the plugin handles it. Otherwise, fall
/// through to built-in handlers.
pub fn resolve_handler<'a>(step: &ActionStep, plugins: &'a PluginRegistry) -> HandlerKind<'a> {
  if step.is_run_step() {
    return HandlerKind::Script;
  }

  let Some(using) = step.runs_using() else {
    return HandlerKind::Script;
  };

  // Plugins take priority over built-in handlers
  if let Some(plugin) = plugins.find(using) {
    return HandlerKind::Plugin(plugin.name());
  }

  match using {
    "script" => HandlerKind::Script,
    "node12" | "node16" | "node20" | "node24" => HandlerKind::Node,
    "docker" => HandlerKind::Docker,
    "composite" => HandlerKind::Composite,
    other => HandlerKind::Unknown(other.to_owned()),
  }
}
