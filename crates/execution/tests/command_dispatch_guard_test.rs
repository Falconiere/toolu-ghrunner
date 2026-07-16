//! FIX 1 & 2 (CVE-2020-15228): stdout `::set-env::` / `::add-path::` are
//! refused. A step's untrusted stdout must not mutate the runner's live
//! env/PATH for later steps; the dispatcher warns once and applies nothing.
//! The only sanctioned path is the `$GITHUB_ENV` / `$GITHUB_PATH` files.
//!
//! Real `CommandDispatcher` + real `ExecutionContext`, no mocks.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use execution::execution::command_dispatch::CommandDispatcher;
use execution::execution::context::ExecutionContext;
use shared::{AnnotationLevel, RunnerEvent, SecretMasker};

/// Feed one stdout line to a fresh dispatcher; return the mutated ctx and any
/// Warning-annotation messages the dispatcher queued.
fn dispatch_one(line: &str) -> (ExecutionContext, Vec<String>) {
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let mut ctx = ExecutionContext::with_masker(Arc::clone(&masker));
  let mut d = CommandDispatcher::new("s1", masker);
  d.on_stdout_line(line, &mut ctx);
  let warnings = d
    .take_events()
    .into_iter()
    .filter_map(|e| {
      if let RunnerEvent::Annotation {
        level: AnnotationLevel::Warning,
        message,
        ..
      } = e
      {
        Some(message)
      } else {
        None
      }
    })
    .collect();
  (ctx, warnings)
}

#[test]
fn stdout_set_env_is_refused_and_does_not_mutate_ctx() {
  // The vulnerable behavior would set LD_PRELOAD for every later step.
  let (ctx, warnings) = dispatch_one("::set-env name=LD_PRELOAD::/tmp/evil.so");

  assert!(
    ctx.env_var("LD_PRELOAD").is_none(),
    "stdout ::set-env:: must NOT mutate the execution env (CVE-2020-15228)"
  );
  assert_eq!(warnings.len(), 1, "exactly one warning expected; got {warnings:?}");
  let msg = warnings.first().map(String::as_str).unwrap_or_default();
  assert!(
    msg.contains("set-env") && msg.contains("GITHUB_ENV"),
    "warning must name set-env and $GITHUB_ENV; got {msg:?}"
  );
}

#[test]
fn stdout_add_path_is_refused_and_does_not_change_path() {
  // If add-path had run, path_additions would make build_step_env surface a
  // PATH that begins with the attacker dir; refused, it stays absent.
  let (ctx, warnings) = dispatch_one("::add-path::/opt/attacker/bin");

  let env = ctx.build_step_env(&HashMap::new());
  let path = env.get("PATH").cloned().unwrap_or_default();
  assert!(
    !path.contains("/opt/attacker/bin"),
    "stdout ::add-path:: must NOT prepend to PATH; PATH={path:?}"
  );
  assert_eq!(warnings.len(), 1, "exactly one warning expected; got {warnings:?}");
  let msg = warnings.first().map(String::as_str).unwrap_or_default();
  assert!(
    msg.contains("add-path") && msg.contains("GITHUB_PATH"),
    "warning must name add-path and $GITHUB_PATH; got {msg:?}"
  );
}

#[test]
fn sibling_stdout_commands_still_apply() {
  // Only set-env/add-path are off — set-output must keep working with no warning.
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let mut ctx = ExecutionContext::with_masker(Arc::clone(&masker));
  let mut d = CommandDispatcher::new("s1", masker);

  d.on_stdout_line("::set-output name=result::ok", &mut ctx);

  assert_eq!(
    ctx.step_outputs("s1").get("result").map(String::as_str),
    Some("ok"),
    "::set-output:: must still set the step output"
  );
  assert!(
    d.take_events().is_empty(),
    "set-output must not emit an annotation"
  );
}
