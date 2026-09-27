//! Pinned setup-node matcher and captured real-tool output checks.
use super::problem_matcher::MatcherRegistry;
use super::problem_matcher_state::{MatchResult, MatcherState};

const TSC: &str = include_str!("problem_matcher_tsc.json");
const TSC_OUTPUT: &str = include_str!("problem_matcher_tsc.txt");
const ESLINT: &str = include_str!("problem_matcher_eslint.json");
const ESLINT_OUTPUT: &str = include_str!("problem_matcher_eslint.txt");

fn match_output(registry: &MatcherRegistry, output: &str) -> Vec<MatchResult> {
  let mut state = MatcherState::default();
  output
    .lines()
    .filter_map(|line| state.match_line(registry, line))
    .collect()
}

#[test]
fn matches_the_captured_typescript_output() -> Result<(), String> {
  let mut registry = MatcherRegistry::default();
  registry.add_json(TSC)?;

  let results = match_output(&registry, TSC_OUTPUT);
  assert_eq!(results.len(), 1);
  let result = results
    .first()
    .ok_or_else(|| "missing TypeScript match".to_owned())?;
  assert_eq!(result.file.as_deref(), Some("src/example.ts"));
  assert_eq!(result.line.as_deref(), Some("1"));
  assert_eq!(result.column.as_deref(), Some("7"));
  assert_eq!(result.severity.as_deref(), Some("error"));
  assert_eq!(result.code.as_deref(), Some("2322"));
  assert_eq!(
    result.message.as_deref(),
    Some("Type 'number' is not assignable to type 'string'.")
  );
  Ok(())
}

#[test]
fn matches_the_captured_looping_eslint_output() -> Result<(), String> {
  let mut registry = MatcherRegistry::default();
  registry.add_json(ESLINT)?;

  let results = match_output(&registry, ESLINT_OUTPUT);
  assert_eq!(results.len(), 2);
  let first = results
    .first()
    .ok_or_else(|| "missing first ESLint match".to_owned())?;
  let second = results
    .get(1)
    .ok_or_else(|| "missing second ESLint match".to_owned())?;
  assert_eq!(first.file.as_deref(), Some("@WORKSPACE@/src/example.js"));
  assert_eq!(first.line.as_deref(), Some("1"));
  assert_eq!(first.column.as_deref(), Some("5"));
  assert_eq!(first.severity.as_deref(), Some("warning"));
  assert_eq!(first.code.as_deref(), Some("no-unused-vars"));
  assert_eq!(second.file.as_deref(), Some("@WORKSPACE@/src/example.js"));
  assert_eq!(second.line.as_deref(), Some("2"));
  assert_eq!(second.column.as_deref(), Some("13"));
  assert_eq!(second.severity.as_deref(), Some("error"));
  assert_eq!(second.code.as_deref(), Some("no-undef"));
  Ok(())
}

#[test]
fn removes_captured_matcher_case_insensitively() -> Result<(), String> {
  let mut registry = MatcherRegistry::default();
  registry.add_json(ESLINT)?;
  registry.remove_owner("ESLINT-STYLISH");

  assert!(match_output(&registry, ESLINT_OUTPUT).is_empty());
  Ok(())
}

#[test]
fn malformed_captured_matcher_does_not_replace_an_owner() -> Result<(), String> {
  let mut registry = MatcherRegistry::default();
  registry.add_json(TSC)?;
  let malformed = TSC.replace("\"message\": 6", "\"message\": 99");
  assert!(registry.add_json(&malformed).is_err());

  assert_eq!(match_output(&registry, TSC_OUTPUT).len(), 1);
  Ok(())
}

#[test]
fn matches_colored_captured_typescript_output_and_resets_long_lines() -> Result<(), String> {
  let mut registry = MatcherRegistry::default();
  registry.add_json(TSC)?;
  let mut state = MatcherState::default();
  let colored = format!("\u{1b}[31m{}\u{1b}[0m", TSC_OUTPUT.trim());
  assert!(state.match_line(&registry, &colored).is_some());
  let long_line = "x".repeat(64 * 1024 + 1);
  assert!(state.match_line(&registry, &long_line).is_none());
  Ok(())
}

#[test]
fn replacement_resets_only_that_owner_and_file_removal_ignores_patterns() -> Result<(), String> {
  let mut registry = MatcherRegistry::default();
  registry.add_json(ESLINT)?;
  let mut state = MatcherState::default();
  let mut lines = ESLINT_OUTPUT.lines();
  let _ = lines.next();
  let file = lines.next().ok_or("missing real ESLint filename")?;
  let diagnostic = lines.next().ok_or("missing real ESLint diagnostic")?;
  assert!(state.match_line(&registry, file).is_none());
  registry.add_json(TSC)?;
  assert!(state.match_line(&registry, diagnostic).is_some());
  registry.add_json(ESLINT)?;
  assert!(state.match_line(&registry, diagnostic).is_none());
  let invalid_patterns = TSC.replace("\"message\": 6", "\"message\": 99");
  registry.remove_json(&invalid_patterns)?;
  assert!(match_output(&registry, TSC_OUTPUT).is_empty());
  Ok(())
}

#[test]
fn validates_capture_schema_and_rejects_pathological_regex() -> Result<(), String> {
  let mut registry = MatcherRegistry::default();
  registry.add_json(TSC)?;
  for invalid in [
    TSC.replace("\"message\": 6", "\"message\": -1"),
    TSC.replace("\"message\": 6", "\"message\": 6, \"loop\": true"),
    TSC.replace(
      "\"owner\": \"tsc\"",
      "\"owner\": \"tsc\", \"severity\": \"info\"",
    ),
    TSC.replace("^([^", "(?=x)^([^"),
    TSC.replace("\"owner\": \"tsc\"", "\"owner\": \"\""),
  ] {
    assert!(registry.add_json(&invalid).is_err());
    assert_eq!(match_output(&registry, TSC_OUTPUT).len(), 1);
  }
  let whole_match = TSC.replace("\"message\": 6", "\"message\": 0");
  registry.add_json(&whole_match)?;
  let results = match_output(&registry, TSC_OUTPUT);
  assert_eq!(
    results.first().and_then(|result| result.message.as_deref()),
    Some(TSC_OUTPUT.trim())
  );
  Ok(())
}

#[test]
fn colored_case_insensitive_severity_and_long_line_reset() -> Result<(), String> {
  let mut registry = MatcherRegistry::default();
  let matcher = TSC.replace("error|warning|info", "ERROR|WARNING|INFO");
  registry.add_json(&matcher)?;
  let actual = TSC_OUTPUT.replace("error TS", "ERROR TS");
  assert_eq!(match_output(&registry, &actual).len(), 1);
  registry.add_json(ESLINT)?;
  let mut state = MatcherState::default();
  let mut lines = ESLINT_OUTPUT.lines();
  let _ = lines.next();
  let file = lines.next().ok_or("missing filename")?;
  let diagnostic = lines.next().ok_or("missing diagnostic")?;
  assert!(state.match_line(&registry, file).is_none());
  let long_line = TSC_OUTPUT.repeat(1024);
  assert!(long_line.len() > 64 * 1024);
  assert!(state.match_line(&registry, &long_line).is_none());
  assert!(state.match_line(&registry, diagnostic).is_none());
  Ok(())
}
