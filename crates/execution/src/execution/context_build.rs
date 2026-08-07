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
  crate::config::var(key).is_some_and(|v| {
    let v = v.trim();
    v == "1" || v.eq_ignore_ascii_case("true")
  })
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
  s.insert(
    "job-index".to_owned(),
    ExprValue::Number(matrix_count_to_f64(job_index)),
  );
  s.insert(
    "job-total".to_owned(),
    ExprValue::Number(matrix_count_to_f64(job_total)),
  );
  let max = max_parallel.map_or(ExprValue::Null, |m| {
    ExprValue::Number(matrix_count_to_f64(m))
  });
  s.insert("max-parallel".to_owned(), max);
  s
}

/// Convert a matrix job-count/index to its `${{ }}` numeric representation.
///
/// GitHub Actions caps matrix jobs at 256 (github.com) / 65536 (GHES max
/// documented ceiling), so every legitimate value here fits a `u32` losslessly
/// as `f64`. Falls back to `f64::from(u32::MAX)` for the practically
/// unreachable case of a count that doesn't fit `u32`, rather than silently
/// losing precision via `as`.
fn matrix_count_to_f64(count: u64) -> f64 {
  u32::try_from(count).map_or(f64::from(u32::MAX), f64::from)
}
