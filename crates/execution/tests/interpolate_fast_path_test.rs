//! T2a: `ExecutionContext::interpolate_string` returns non-`${{`-bearing
//! input without ever building an `eval_context()` snapshot.
//!
//! `expressions::template::interpolate` already returns its input verbatim
//! when `find("${{")` fails; these confirm the early return added in front
//! of it is byte-identical for every input shape that lacks a literal
//! `${{`, including a bare `${` and a lone `}}`, and that expression-bearing
//! input still resolves through the (unskipped) full path.
//!
//! S4 (per-step `EvalContext` reuse): `interpolate_with`/`evaluate_with`
//! evaluate against a caller-supplied snapshot instead of building a fresh
//! one, and carry the SAME `${{`-free fast path. These tests mirror the
//! `interpolate_string` cases above against `interpolate_with` directly, and
//! confirm both variants agree byte-for-byte on the same input.

use execution::execution::context::ExecutionContext;

/// A real (not mocked) context with one github value set the way
/// `job_runner::build_context` populates it, so `${{ github.repository }}`
/// has something real to resolve.
fn ctx_with_repository() -> ExecutionContext {
  let mut ctx = ExecutionContext::new_for_test();
  ctx.set_github_context("repository", "octocat/hello-world");
  ctx
}

#[test]
fn plain_string_returns_byte_identical() {
  let ctx = ctx_with_repository();
  let input = "just a plain string, no expressions here";
  assert_eq!(ctx.interpolate_string(input).unwrap(), input);
}

#[test]
fn bare_dollar_brace_without_the_second_brace_is_left_alone() {
  // `${` alone never forms the `${{` marker, on either the old or new path.
  let ctx = ctx_with_repository();
  let input = "cost is ${5} not an expression";
  assert_eq!(ctx.interpolate_string(input).unwrap(), input);
}

#[test]
fn lone_closing_double_brace_is_left_alone() {
  let ctx = ctx_with_repository();
  let input = "trailing }} with no opener";
  assert_eq!(ctx.interpolate_string(input).unwrap(), input);
}

#[test]
fn empty_string_is_left_alone() {
  let ctx = ctx_with_repository();
  assert_eq!(ctx.interpolate_string("").unwrap(), "");
}

#[test]
fn expression_bearing_input_still_resolves() {
  let ctx = ctx_with_repository();
  let out = ctx
    .interpolate_string("repo: ${{ github.repository }}")
    .unwrap();
  assert_eq!(out, "repo: octocat/hello-world");
}

// ---------------------------------------------------------------------------
// `interpolate_with` / `evaluate_with` (S4): same fast path, same semantics,
// against a caller-supplied snapshot.
// ---------------------------------------------------------------------------

#[test]
fn interpolate_with_plain_string_returns_byte_identical() {
  let ctx = ctx_with_repository();
  let snapshot = ctx.eval_context();
  let input = "just a plain string, no expressions here";
  assert_eq!(ctx.interpolate_with(&snapshot, input).unwrap(), input);
}

#[test]
fn interpolate_with_bare_dollar_brace_is_left_alone() {
  let ctx = ctx_with_repository();
  let snapshot = ctx.eval_context();
  let input = "cost is ${5} not an expression";
  assert_eq!(ctx.interpolate_with(&snapshot, input).unwrap(), input);
}

#[test]
fn interpolate_with_expression_bearing_input_still_resolves() {
  let ctx = ctx_with_repository();
  let snapshot = ctx.eval_context();
  let out = ctx
    .interpolate_with(&snapshot, "repo: ${{ github.repository }}")
    .unwrap();
  assert_eq!(out, "repo: octocat/hello-world");
}

#[test]
fn interpolate_with_and_interpolate_string_agree_byte_for_byte() {
  let ctx = ctx_with_repository();
  let snapshot = ctx.eval_context();
  for input in [
    "no expression here",
    "cost is ${5}",
    "repo: ${{ github.repository }}",
    "",
  ] {
    assert_eq!(
      ctx.interpolate_with(&snapshot, input).unwrap(),
      ctx.interpolate_string(input).unwrap(),
      "interpolate_with must render byte-identically to interpolate_string for {input:?}"
    );
  }
}

#[test]
fn evaluate_with_matches_evaluate_expression() {
  let ctx = ctx_with_repository();
  let snapshot = ctx.eval_context();
  let via_with = ctx
    .evaluate_with(&snapshot, "github.repository")
    .unwrap()
    .coerce_to_string();
  let via_fresh = ctx
    .evaluate_expression("github.repository")
    .unwrap()
    .coerce_to_string();
  assert_eq!(via_with, via_fresh);
  assert_eq!(via_with, "octocat/hello-world");
}
