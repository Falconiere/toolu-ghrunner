//! Real-data tests for cache trust classification and scope resolution.
//!
//! No mocks: `classify_trust` is driven on the exact event/branch matrix,
//! and `scopes_for_job` runs against a real `ExecutionContext` built with
//! its test constructor and populated via `set_github_context`. Assertions
//! flow through `Result`-returning helpers, so each test genuinely uses `?`.

use std::error::Error;
use std::fmt::Debug;

use toolu_runner::execution::cache::scope::scopes_for_job;
use toolu_runner::execution::cache::trust::{TrustLevel, classify_trust};
use toolu_runner::execution::context::ExecutionContext;

/// Build a `Vec<String>` protected-branch list from string literals.
fn protected(branches: &[&str]) -> Vec<String> {
  branches.iter().map(|b| (*b).to_owned()).collect()
}

/// Fail with `what` unless `actual` equals `expected`.
fn eq<T: PartialEq + Debug>(actual: &T, expected: &T, what: &str) -> Result<(), Box<dyn Error>> {
  if actual == expected {
    Ok(())
  } else {
    Err(format!("{what}: expected {expected:?}, got {actual:?}").into())
  }
}

/// Fail with `msg` unless `cond` holds.
fn check(cond: bool, msg: &str) -> Result<(), Box<dyn Error>> {
  if cond { Ok(()) } else { Err(msg.into()) }
}

#[test]
fn classify_trust_matrix() -> Result<(), Box<dyn Error>> {
  let main = protected(&["main"]);

  // Trusting events on a protected branch → Trusted.
  eq(
    &classify_trust("push", "main", &main),
    &TrustLevel::Trusted,
    "push/main",
  )?;
  eq(
    &classify_trust("workflow_dispatch", "main", &main),
    &TrustLevel::Trusted,
    "workflow_dispatch/main",
  )?;
  eq(
    &classify_trust("schedule", "main", &main),
    &TrustLevel::Trusted,
    "schedule/main",
  )?;
  eq(
    &classify_trust("release", "main", &main),
    &TrustLevel::Trusted,
    "release/main",
  )?;

  // Trusting events on a NON-protected branch → Untrusted.
  eq(
    &classify_trust("push", "feature-x", &main),
    &TrustLevel::Untrusted,
    "push/feature-x",
  )?;
  eq(
    &classify_trust("schedule", "feature", &main),
    &TrustLevel::Untrusted,
    "schedule/feature",
  )?;

  // THE BUG FIX: workflow_dispatch on a non-protected branch is NOT trusted.
  eq(
    &classify_trust("workflow_dispatch", "feature-x", &main),
    &TrustLevel::Untrusted,
    "workflow_dispatch/feature-x",
  )?;

  // Non-trusting events are Untrusted even on a protected branch.
  eq(
    &classify_trust("pull_request", "main", &main),
    &TrustLevel::Untrusted,
    "pull_request/main",
  )?;
  eq(
    &classify_trust("pull_request_target", "main", &main),
    &TrustLevel::Untrusted,
    "pull_request_target/main",
  )?;

  Ok(())
}

#[test]
fn scopes_for_pull_request_context() -> Result<(), Box<dyn Error>> {
  let head_ref = "refs/pull/7/merge";
  let mut ctx = ExecutionContext::new_for_test();
  ctx.set_github_context("ref_name", head_ref);
  ctx.set_github_context("base_ref", "main");
  ctx.set_github_context("event_name", "pull_request");

  let scopes = scopes_for_job(&ctx, &protected(&["main"]));

  // Write is the running (head) ref.
  eq(&scopes.write.as_str(), &head_ref, "write")?;
  // Read ladder: head ref, then base ("main"), which is also protected —
  // deduped, so "main" appears once.
  eq(
    &scopes.read_ladder,
    &vec![head_ref.to_owned(), "main".to_owned()],
    "read_ladder",
  )?;
  // A sibling feature branch is never in the ladder.
  check(
    !scopes.read_ladder.iter().any(|s| s == "feature-y"),
    "sibling branch leaked into ladder",
  )?;

  Ok(())
}

#[test]
fn scopes_for_push_context() -> Result<(), Box<dyn Error>> {
  let mut ctx = ExecutionContext::new_for_test();
  ctx.set_github_context("ref_name", "main");
  ctx.set_github_context("event_name", "push");

  let scopes = scopes_for_job(&ctx, &protected(&["main"]));

  // No base_ref; ref_name == protected → single deduped scope.
  eq(&scopes.write.as_str(), &"main", "write")?;
  eq(&scopes.read_ladder, &vec!["main".to_owned()], "read_ladder")?;

  Ok(())
}

#[test]
fn scopes_with_no_ref_name() -> Result<(), Box<dyn Error>> {
  let ctx = ExecutionContext::new_for_test();
  let prot = protected(&["main", "master"]);

  let scopes = scopes_for_job(&ctx, &prot);

  // A ref-less job writes nothing but can still read the default scopes.
  eq(&scopes.write.as_str(), &"", "write")?;
  eq(&scopes.read_ladder, &prot, "read_ladder")?;

  Ok(())
}
