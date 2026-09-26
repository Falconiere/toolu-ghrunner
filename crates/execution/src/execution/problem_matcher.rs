//! Problem-matcher registration with validated, atomic replacement.

use std::sync::Arc;

use super::problem_matcher_model::{MAX_OWNERS, MatcherDefinition, parse_add, parse_remove};

/// Job-wide, ordered problem-matcher definitions.
#[derive(Clone, Default)]
pub(crate) struct MatcherRegistry {
  definitions: Vec<Arc<MatcherDefinition>>,
}

impl MatcherRegistry {
  /// Add definitions from a matcher JSON document, replacing matching owners atomically.
  pub(crate) fn add_json(&mut self, input: &str) -> Result<(), String> {
    let additions = parse_add(input)?;
    let mut definitions = self.definitions.clone();
    for addition in additions.iter().rev() {
      definitions.retain(|existing| existing.owner_key != addition.owner_key);
      definitions.insert(0, Arc::clone(addition));
    }
    if definitions.len() > MAX_OWNERS {
      return Err("problem matcher registry has too many owners".to_owned());
    }
    self.definitions = definitions;
    Ok(())
  }

  /// Remove every owner named by a matcher JSON document without validating its patterns.
  pub(crate) fn remove_json(&mut self, input: &str) -> Result<(), String> {
    let owners = parse_remove(input)?;
    self
      .definitions
      .retain(|definition| !owners.contains(&definition.owner_key));
    Ok(())
  }

  /// Remove one owner, matching its name case insensitively.
  pub(crate) fn remove_owner(&mut self, owner: &str) {
    let owner = owner.trim().to_ascii_lowercase();
    if !owner.is_empty() {
      self
        .definitions
        .retain(|definition| definition.owner_key != owner);
    }
  }

  /// Borrow definitions in their matching-priority order.
  pub(super) fn definitions(&self) -> &[Arc<MatcherDefinition>] {
    &self.definitions
  }
}
