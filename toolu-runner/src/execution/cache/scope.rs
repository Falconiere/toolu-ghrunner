//! Read-ladder and write-scope resolution for the cache index.
//!
//! The write scope is the job's own ref; the read ladder walks own ref →
//! PR base branch (when present) → protected/default branches, so a job
//! sees its own entries, then its base's, then the shared defaults — never
//! a sibling branch's.

use crate::execution::context::ExecutionContext;

/// The index scopes a job may write and read.
#[derive(Clone)]
pub struct CacheScopes {
  /// Scope written by this job — its own `github.ref_name`.
  pub write: String,
  /// Scopes searched on read, most specific first, first-occurrence deduped.
  pub read_ladder: Vec<String>,
}

/// Resolve cache scopes from the job's github context.
///
/// `write` is `github.ref_name` (the running ref). `read_ladder` is
/// `dedup([ref_name, base_ref?, ...protected])`: own ref, then the PR
/// base branch when the context carries one, then the protected branches.
/// With no `ref_name`, `write` is empty and the ladder is just `protected`
/// (a ref-less job can still read the default scope).
pub fn scopes_for_job(ctx: &ExecutionContext, protected: &[String]) -> CacheScopes {
  let ref_name = ctx.github_context("ref_name");
  let base_ref = ctx.github_context("base_ref");

  let mut read_ladder: Vec<String> = Vec::new();
  if let Some(name) = ref_name {
    push_unique(&mut read_ladder, name.to_owned());
  }
  if let Some(base) = base_ref {
    push_unique(&mut read_ladder, base.to_owned());
  }
  for scope in protected {
    push_unique(&mut read_ladder, scope.clone());
  }

  CacheScopes {
    write: ref_name.unwrap_or_default().to_owned(),
    read_ladder,
  }
}

/// Append `value` to `ladder` only if it is not already present.
fn push_unique(ladder: &mut Vec<String>, value: String) {
  if !ladder.contains(&value) {
    ladder.push(value);
  }
}
