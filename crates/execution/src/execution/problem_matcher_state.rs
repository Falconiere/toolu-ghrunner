//! Per-output-stream multiline state for problem matchers.

use std::collections::HashMap;
use std::sync::Arc;

use regex::Captures;

use super::problem_matcher::MatcherRegistry;
use super::problem_matcher_model::{CaptureFields, MatcherDefinition};

const MAX_LINE_BYTES: usize = 64 * 1024;

/// A diagnostic extracted from a problem-matcher output line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct MatchResult {
  pub(crate) file: Option<String>,
  pub(crate) line: Option<String>,
  pub(crate) column: Option<String>,
  pub(crate) severity: Option<String>,
  pub(crate) code: Option<String>,
  pub(crate) message: Option<String>,
  pub(crate) from_path: Option<String>,
}

/// Mutable multiline progress for one output stream.
#[derive(Default)]
pub(crate) struct MatcherState {
  matchers: HashMap<String, ActiveMatcher>,
}

struct ActiveMatcher {
  definition: Arc<MatcherDefinition>,
  states: Vec<Option<MatchResult>>,
}

impl MatcherState {
  /// Match one output line against the registry's priority-ordered definitions.
  pub(crate) fn match_line(
    &mut self,
    registry: &MatcherRegistry,
    line: &str,
  ) -> Option<MatchResult> {
    self.sync(registry);
    if registry.definitions().is_empty() {
      return None;
    }
    if line.len() > MAX_LINE_BYTES {
      self.reset();
      return None;
    }
    let line = strip_ansi(line);
    for definition in registry.definitions() {
      let Some(active) = self.matchers.get_mut(&definition.owner_key) else {
        continue;
      };
      if let Some(result) = match_active(active, &line) {
        self.reset_except(&definition.owner_key);
        if is_reportable(&result) {
          return Some(result);
        }
      }
    }
    None
  }

  /// Discard all partial multiline matches.
  pub(crate) fn reset(&mut self) {
    for active in self.matchers.values_mut() {
      reset_active(active);
    }
  }

  fn sync(&mut self, registry: &MatcherRegistry) {
    self.matchers.retain(|owner, active| {
      registry.definitions().iter().any(|definition| {
        definition.owner_key == *owner && Arc::ptr_eq(definition, &active.definition)
      })
    });
    for definition in registry.definitions() {
      self
        .matchers
        .entry(definition.owner_key.clone())
        .or_insert_with(|| ActiveMatcher {
          definition: Arc::clone(definition),
          states: vec![None; definition.patterns.len()],
        });
    }
  }

  fn reset_except(&mut self, owner: &str) {
    for (candidate, active) in &mut self.matchers {
      if candidate != owner {
        reset_active(active);
      }
    }
  }
}

fn apply_defaults(result: &mut MatchResult, definition: &MatcherDefinition) {
  if result.severity.as_deref().is_none_or(str::is_empty) {
    result.severity.clone_from(&definition.severity);
  }
  if result.from_path.as_deref().is_none_or(str::is_empty) {
    result.from_path.clone_from(&definition.from_path);
  }
}

fn reset_active(active: &mut ActiveMatcher) {
  active.states.fill(None);
}

fn match_active(active: &mut ActiveMatcher, line: &str) -> Option<MatchResult> {
  let last = active.definition.patterns.len().checked_sub(1)?;
  for index in (0..=last).rev() {
    if let Some(result) = attempt_pattern(active, index, last, line) {
      return Some(result);
    }
  }
  None
}

fn attempt_pattern(
  active: &mut ActiveMatcher,
  index: usize,
  last: usize,
  line: &str,
) -> Option<MatchResult> {
  let predecessor = index.checked_sub(1);
  if predecessor.is_some_and(|previous| active.states.get(previous).is_none_or(Option::is_none)) {
    return None;
  }
  let pattern = active.definition.patterns.get(index)?;
  let Some(captures) = pattern.regex.captures(line) else {
    clear_failed_state(active, index, last, predecessor);
    return None;
  };
  let fields = pattern.captures.clone();
  let loop_pattern = pattern.loop_pattern;
  let mut result = previous_result(active, predecessor)?;
  apply_captures(&mut result, &fields, &captures);
  if index != last {
    if let Some(state) = active.states.get_mut(index) {
      *state = Some(result);
    }
    return None;
  }
  apply_defaults(&mut result, &active.definition);
  finish_match(active, last, loop_pattern);
  Some(result)
}

fn clear_failed_state(
  active: &mut ActiveMatcher,
  index: usize,
  last: usize,
  predecessor: Option<usize>,
) {
  let reset_index = if index == last {
    predecessor
  } else {
    Some(index)
  };
  if let Some(state) = reset_index.and_then(|reset_index| active.states.get_mut(reset_index)) {
    *state = None;
  }
}

fn previous_result(active: &ActiveMatcher, predecessor: Option<usize>) -> Option<MatchResult> {
  match predecessor {
    Some(previous) => active.states.get(previous)?.clone(),
    None => Some(MatchResult::default()),
  }
}

fn finish_match(active: &mut ActiveMatcher, last: usize, loop_pattern: bool) {
  if loop_pattern && last > 0 {
    let preserved = last
      .checked_sub(1)
      .and_then(|previous| active.states.get(previous))
      .cloned()
      .flatten();
    active.states.fill(None);
    if let Some(state) = last
      .checked_sub(1)
      .and_then(|previous| active.states.get_mut(previous))
    {
      *state = preserved;
    }
  } else {
    reset_active(active);
  }
}

fn is_reportable(result: &MatchResult) -> bool {
  let message_ok = result
    .message
    .as_deref()
    .is_some_and(|message| !message.trim().is_empty());
  let severity_ok = result.severity.as_deref().is_none_or(|severity| {
    ["", "error", "warning", "notice"]
      .iter()
      .any(|value| severity.eq_ignore_ascii_case(value))
  });
  message_ok && severity_ok
}

fn apply_captures(result: &mut MatchResult, fields: &CaptureFields, captures: &Captures<'_>) {
  assign(&mut result.file, fields.file, captures);
  assign(&mut result.line, fields.line, captures);
  assign(&mut result.column, fields.column, captures);
  assign(&mut result.severity, fields.severity, captures);
  assign(&mut result.code, fields.code, captures);
  assign(&mut result.message, fields.message, captures);
  assign(&mut result.from_path, fields.from_path, captures);
}

fn assign(target: &mut Option<String>, index: Option<usize>, captures: &Captures<'_>) {
  if let Some(value) = index.and_then(|index| captures.get(index)) {
    *target = Some(value.as_str().to_owned());
  }
}

fn strip_ansi(line: &str) -> String {
  let mut output = String::with_capacity(line.len());
  let mut characters = line.chars().peekable();
  while let Some(character) = characters.next() {
    if character == '\u{1b}' && characters.peek() == Some(&'[') {
      let _ = characters.next();
      for character in characters.by_ref() {
        if character.is_ascii() && ('@'..='~').contains(&character) {
          break;
        }
      }
    } else {
      output.push(character);
    }
  }
  output
}
