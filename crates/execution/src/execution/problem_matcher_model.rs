//! Parsed and validated GitHub Actions problem-matcher definitions.

use std::sync::Arc;

use regex::{Regex, RegexBuilder};
use serde::Deserialize;

/// Maximum accepted matcher-document size.
pub(super) const MAX_JSON_BYTES: usize = 1024 * 1024;
/// Maximum owners retained in a job registry.
pub(super) const MAX_OWNERS: usize = 64;
const MAX_PATTERNS: usize = 32;
const MAX_REGEX_BYTES: usize = 16 * 1024;
const MAX_REGEX_SIZE: usize = 256 * 1024;

/// A compiled matcher definition kept by the registry.
#[derive(Debug)]
pub(super) struct MatcherDefinition {
  pub(super) owner_key: String,
  pub(super) severity: Option<String>,
  pub(super) from_path: Option<String>,
  pub(super) patterns: Vec<PatternDefinition>,
}

/// One regular-expression stage in a matcher definition.
#[derive(Debug)]
pub(super) struct PatternDefinition {
  pub(super) regex: Regex,
  pub(super) captures: CaptureFields,
  pub(super) loop_pattern: bool,
}

/// Capture-group indices assigned to annotation fields.
#[derive(Debug, Clone, Default)]
pub(super) struct CaptureFields {
  pub(super) file: Option<usize>,
  pub(super) line: Option<usize>,
  pub(super) column: Option<usize>,
  pub(super) severity: Option<usize>,
  pub(super) code: Option<usize>,
  pub(super) message: Option<usize>,
  pub(super) from_path: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct MatcherFile {
  #[serde(rename = "problemMatcher")]
  matchers: Option<Vec<RawMatcher>>,
}

#[derive(Debug, Deserialize)]
struct RemovalFile {
  #[serde(rename = "problemMatcher")]
  matchers: Option<Vec<RemovalMatcher>>,
}

#[derive(Debug, Deserialize)]
struct RemovalMatcher {
  owner: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawMatcher {
  owner: Option<String>,
  severity: Option<String>,
  #[serde(rename = "fromPath")]
  from_path: Option<String>,
  pattern: Option<Vec<RawPattern>>,
}

#[derive(Debug, Deserialize)]
struct RawPattern {
  regexp: Option<String>,
  file: Option<usize>,
  line: Option<usize>,
  column: Option<usize>,
  severity: Option<usize>,
  code: Option<usize>,
  message: Option<usize>,
  #[serde(rename = "fromPath")]
  from_path: Option<usize>,
  #[serde(rename = "loop", default)]
  loop_pattern: bool,
}

/// Parse a matcher document and compile all of its patterns.
pub(super) fn parse_add(input: &str) -> Result<Vec<Arc<MatcherDefinition>>, String> {
  if input.len() > MAX_JSON_BYTES {
    return Err("problem matcher JSON exceeds the size limit".to_owned());
  }
  let file = parse_file(input)?;
  let matchers = file.matchers.unwrap_or_default();
  if matchers.len() > MAX_OWNERS {
    return Err("problem matcher registry has too many owners".to_owned());
  }
  let mut definitions = Vec::with_capacity(matchers.len());
  let mut owners = Vec::with_capacity(matchers.len());
  for matcher in matchers {
    let definition = parse_matcher(matcher)?;
    if owners.iter().any(|owner| owner == &definition.owner_key) {
      return Err("problem matcher owners must be unique".to_owned());
    }
    owners.push(definition.owner_key.clone());
    definitions.push(Arc::new(definition));
  }
  Ok(definitions)
}

/// Parse owner names from a matcher document without compiling its patterns.
pub(super) fn parse_remove(input: &str) -> Result<Vec<String>, String> {
  if input.len() > MAX_JSON_BYTES {
    return Err("problem matcher JSON exceeds the size limit".to_owned());
  }
  let file: RemovalFile = serde_json::from_str(input).map_err(|error| json_error(&error))?;
  let mut owners = Vec::new();
  for matcher in file.matchers.unwrap_or_default() {
    let Some(owner) = matcher.owner else {
      continue;
    };
    let owner = owner.trim();
    if owner.is_empty() {
      continue;
    }
    let owner_key = owner.to_ascii_lowercase();
    if !owners.iter().any(|existing| existing == &owner_key) {
      owners.push(owner_key);
    }
  }
  Ok(owners)
}

fn parse_file(input: &str) -> Result<MatcherFile, String> {
  serde_json::from_str(input).map_err(|error| json_error(&error))
}

fn json_error(error: &serde_json::Error) -> String {
  format!(
    "invalid problem matcher JSON at {}:{}",
    error.line(),
    error.column()
  )
}

fn parse_matcher(raw: RawMatcher) -> Result<MatcherDefinition, String> {
  let owner = raw
    .owner
    .map(|value| value.trim().to_owned())
    .filter(|value| !value.is_empty())
    .ok_or_else(|| "problem matcher owner is required".to_owned())?;
  let severity = validate_severity(raw.severity)?;
  let patterns = raw
    .pattern
    .ok_or_else(|| "problem matcher pattern is required".to_owned())?;
  if patterns.is_empty() || patterns.len() > MAX_PATTERNS {
    return Err("problem matcher has an invalid number of patterns".to_owned());
  }
  if patterns.len() == 1 && patterns.iter().any(|pattern| pattern.loop_pattern) {
    return Err("a single problem matcher pattern may not loop".to_owned());
  }
  let Some(last_pattern) = patterns.len().checked_sub(1) else {
    return Err("problem matcher has an invalid number of patterns".to_owned());
  };
  let mut definitions = Vec::with_capacity(patterns.len());
  let mut assigned_fields = [false; 7];
  for (index, pattern) in patterns.into_iter().enumerate() {
    let definition = parse_pattern(pattern, index == last_pattern)?;
    validate_field_assignments(&definition.captures, &mut assigned_fields)?;
    definitions.push(definition);
  }
  if !definitions
    .iter()
    .any(|pattern| pattern.captures.message.is_some())
  {
    return Err("a problem matcher pattern must capture a message".to_owned());
  }
  Ok(MatcherDefinition {
    owner_key: owner.to_ascii_lowercase(),
    severity,
    from_path: raw.from_path,
    patterns: definitions,
  })
}

fn validate_severity(severity: Option<String>) -> Result<Option<String>, String> {
  let Some(severity) = severity else {
    return Ok(None);
  };
  let normalized = severity.to_ascii_lowercase();
  match normalized.as_str() {
    "" | "error" | "warning" | "notice" => Ok(Some(normalized)),
    _ => Err("problem matcher severity is invalid".to_owned()),
  }
}

fn parse_pattern(raw: RawPattern, is_last: bool) -> Result<PatternDefinition, String> {
  let source = raw.regexp.unwrap_or_default();
  if source.len() > MAX_REGEX_BYTES {
    return Err("problem matcher regular expression exceeds the size limit".to_owned());
  }
  let regex = RegexBuilder::new(&source)
    .size_limit(MAX_REGEX_SIZE)
    .build()
    // Regex errors quote the pattern, which can contain unregistered secrets.
    .map_err(|_pattern_error| "problem matcher regular expression is unsupported".to_owned())?;
  let captures = CaptureFields {
    file: raw.file,
    line: raw.line,
    column: raw.column,
    severity: raw.severity,
    code: raw.code,
    message: raw.message,
    from_path: raw.from_path,
  };
  validate_capture_fields(&captures, regex.captures_len())?;
  if raw.loop_pattern && !is_last {
    return Err("only the final problem matcher pattern may loop".to_owned());
  }
  if raw.loop_pattern && captures.message.is_none() {
    return Err("a looping problem matcher pattern must capture a message".to_owned());
  }
  Ok(PatternDefinition {
    regex,
    captures,
    loop_pattern: raw.loop_pattern,
  })
}

fn validate_capture_fields(fields: &CaptureFields, captures_len: usize) -> Result<(), String> {
  let indices = [
    fields.file,
    fields.line,
    fields.column,
    fields.severity,
    fields.code,
    fields.message,
    fields.from_path,
  ];
  for index in indices.into_iter().flatten() {
    if index >= captures_len {
      return Err("problem matcher capture index is out of range".to_owned());
    }
  }
  Ok(())
}

fn validate_field_assignments(
  fields: &CaptureFields,
  assigned_fields: &mut [bool; 7],
) -> Result<(), String> {
  let assigned = [
    fields.file.is_some(),
    fields.line.is_some(),
    fields.column.is_some(),
    fields.severity.is_some(),
    fields.code.is_some(),
    fields.message.is_some(),
    fields.from_path.is_some(),
  ];
  for (seen, assigned) in assigned_fields.iter_mut().zip(assigned) {
    if *seen && assigned {
      return Err("a problem matcher field may only be assigned once".to_owned());
    }
    *seen |= assigned;
  }
  Ok(())
}
