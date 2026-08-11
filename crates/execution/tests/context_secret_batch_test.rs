//! Batch secret/masking registration equivalence for `ExecutionContext`
//! (AC-8): `register_secrets`/`add_masks` must mask a fixture line
//! identically to the sequential `register_secret`/`add_mask` calls they
//! replace in `job_runner::register_message_variables` /
//! `job_runner::build_context`.

use execution::execution::context::ExecutionContext;
use expressions::types::ExprValue;
use shared::SecretMasker;

/// Read the masked form of `line` through `ctx`'s shared masker, recovering
/// from a poisoned lock the same way production code does.
fn mask_line(ctx: &ExecutionContext, line: &str) -> String {
  let masker = ctx.masker();
  let guard = match masker.lock() {
    Ok(g) => g,
    Err(poisoned) => poisoned.into_inner(),
  };
  guard.mask(line).into_owned()
}

#[test]
fn register_secrets_batch_matches_fifty_sequential_register_secret_calls() {
  let pairs: Vec<(String, String)> = (0..50)
    .map(|i| (format!("secret-key-{i:03}"), format!("secret-value-{i:03}")))
    .collect();

  let mut batched = ExecutionContext::new_for_test();
  batched.register_secrets(pairs.clone());

  let mut sequential = ExecutionContext::new_for_test();
  for (key, value) in &pairs {
    sequential.register_secret(key, value);
  }

  // Deterministic fixture values (not indexed off the vec — `indexing_slicing`
  // is denied even in tests) touching the first, middle, and last generated value.
  let line = "job env holds secret-value-000, secret-value-024, \
    and secret-value-049 amid plain text";
  assert_eq!(
    mask_line(&batched, line),
    mask_line(&sequential, line),
    "batch and sequential secret registration must mask identically"
  );
}

#[test]
fn add_masks_secret_hints_batch_matches_fifty_sequential_add_mask_calls() {
  let values: Vec<String> = (0..50).map(|i| format!("mask-hint-{i:03}")).collect();

  let mut batched = ExecutionContext::new_for_test();
  batched.add_masks(values.iter().map(String::as_str));

  let mut sequential = ExecutionContext::new_for_test();
  for value in &values {
    sequential.add_mask(value);
  }

  let line = "runtime forwarded mask-hint-000, mask-hint-024, \
    and mask-hint-049 amid plain text";
  assert_eq!(
    mask_line(&batched, line),
    mask_line(&sequential, line),
    "batch and sequential mask-hint registration must mask identically"
  );
}

/// `register_secrets` also inserts every pair into the `secrets.*` context,
/// exactly like `register_secret` — batching must not drop that half of
/// the contract.
#[test]
fn register_secrets_batch_still_populates_secrets_context() {
  let mut ctx = ExecutionContext::new_for_test();
  ctx.register_secrets(vec![
    (
      "token_one".to_owned(),
      "batched-secret-value-one".to_owned(),
    ),
    (
      "token_two".to_owned(),
      "batched-secret-value-two".to_owned(),
    ),
  ]);

  let evaluated = ctx.evaluate_expression("secrets.token_one");
  let surfaced = matches!(&evaluated, Ok(v) if v.to_string() == "batched-secret-value-one");
  assert!(
    surfaced,
    "batched pair must surface in the secrets.* context, got: {evaluated:?}"
  );
}

/// `add_masks` masks the shared masker only — it must NOT surface the
/// value in `secrets.*` (matches `add_mask`'s single-purpose contract,
/// used for the runtime service token and job-message mask hints).
#[test]
fn add_masks_batch_does_not_populate_secrets_context() {
  let mut ctx = ExecutionContext::new_for_test();
  ctx.add_masks(vec!["mask-only-value-not-a-secret"]);

  // `secrets.*` stays empty: `add_masks` never touches the secrets map.
  let secrets = ctx.eval_context().contexts.remove("secrets");
  let secrets_is_empty = matches!(secrets, Some(ExprValue::Object(ref map)) if map.is_empty());
  assert!(
    secrets_is_empty,
    "add_masks must not register a secrets.* entry, got: {secrets:?}"
  );

  // But the value IS masked.
  let masker: &std::sync::Arc<std::sync::Mutex<SecretMasker>> = ctx.masker();
  let guard = match masker.lock() {
    Ok(g) => g,
    Err(poisoned) => poisoned.into_inner(),
  };
  let out = guard.mask("leaking mask-only-value-not-a-secret here");
  assert!(
    !out.contains("mask-only-value-not-a-secret"),
    "add_masks value must still be masked: {out}"
  );
}
