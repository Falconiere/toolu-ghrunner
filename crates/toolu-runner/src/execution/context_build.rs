//! Host/config-derived context assembly helpers for `ExecutionContext`.
//!
//! Pure functions that compute the `runner.*` os/arch, the step-debug flag,
//! and the `strategy.*` object. Kept out of `context.rs` so its `impl`
//! blocks stay under the size ceiling.

use std::collections::HashMap;

use expressions::types::ExprValue;

/// True when step-debug logging is requested via `RUNNER_DEBUG` /
/// `ACTIONS_STEP_DEBUG` (`runner.debug == "1"`).
pub(super) fn runner_debug_on() -> bool {
  is_debug_env("RUNNER_DEBUG") || is_debug_env("ACTIONS_STEP_DEBUG")
}

fn is_debug_env(key: &str) -> bool {
  std::env::var(key)
    .map(|v| {
      let v = v.trim();
      v == "1" || v.eq_ignore_ascii_case("true")
    })
    .unwrap_or(false)
}

/// `strategy.*` for a non-matrix single-job run.
pub(super) fn default_strategy() -> HashMap<String, ExprValue> {
  build_strategy(0, 1, true, None)
}

/// Build the `strategy.*` object from matrix/strategy parameters.
///
/// `max-parallel` is `null` (omitted) when the workflow does not pin it.
pub(super) fn build_strategy(
  job_index: u64,
  job_total: u64,
  fail_fast: bool,
  max_parallel: Option<u64>,
) -> HashMap<String, ExprValue> {
  let mut s = HashMap::new();
  s.insert("fail-fast".to_owned(), ExprValue::Bool(fail_fast));
  s.insert("job-index".to_owned(), ExprValue::Number(job_index as f64));
  s.insert("job-total".to_owned(), ExprValue::Number(job_total as f64));
  let max = max_parallel.map_or(ExprValue::Null, |m| ExprValue::Number(m as f64));
  s.insert("max-parallel".to_owned(), max);
  s
}
