//! Plugin registry for compiled-in runner plugins.

use super::trait_def::RunnerPlugin;

/// Registry holding all compiled-in plugins.
///
/// Plugins are stored in insertion order. If a plugin with the same name
/// is registered twice, the previous one is replaced.
pub struct PluginRegistry {
  plugins: Vec<Box<dyn RunnerPlugin>>,
}

impl PluginRegistry {
  pub fn new() -> Self {
    Self {
      plugins: Vec::new(),
    }
  }

  /// Register a plugin. Replaces any existing plugin with the same name.
  pub fn register(&mut self, plugin: Box<dyn RunnerPlugin>) {
    let name = plugin.name().to_owned();
    self.plugins.retain(|p| p.name() != name);
    self.plugins.push(plugin);
  }

  /// Find a plugin by name.
  pub fn find(&self, name: &str) -> Option<&dyn RunnerPlugin> {
    self
      .plugins
      .iter()
      .find(|p| p.name() == name)
      .map(AsRef::as_ref)
  }

  /// Number of registered plugins.
  pub fn len(&self) -> usize {
    self.plugins.len()
  }

  /// Whether the registry has no plugins.
  pub fn is_empty(&self) -> bool {
    self.plugins.is_empty()
  }

  /// Iterate over all registered plugins in registration order.
  pub fn iter(&self) -> impl Iterator<Item = &dyn RunnerPlugin> {
    self.plugins.iter().map(AsRef::as_ref)
  }
}

impl Default for PluginRegistry {
  fn default() -> Self {
    Self::new()
  }
}
