//! T2a: `ExecutionContext::interpolate_string` returns non-`${{`-bearing
//! input without ever building an `eval_context()` snapshot.
//!
//! `expressions::template::interpolate` already returns its input verbatim
//! when `find("${{")` fails; these confirm the early return added in front
//! of it is byte-identical for every input shape that lacks a literal
//! `${{`, including a bare `${` and a lone `}}`, and that expression-bearing
//! input still resolves through the (unskipped) full path.

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
