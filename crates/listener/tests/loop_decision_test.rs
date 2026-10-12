//! Table-driven tests for `listener::loop_decision::next_action` (AC-4).
//!
//! Real data only: every case resolves to a real `RunnerError` variant and
//! exercises one branch of the normative decision order, including the
//! precedence between `cancelled`, `once`, and `pending_remove`.

use listener::loop_decision::{EphemeralAction, LoopAction, next_action, next_ephemeral_action};
use shared::{Conclusion, RunnerError};

/// A listener outcome marker, kept compact in the table and resolved to a real
/// `RunnerError` at assertion time.
enum Outcome {
  /// Clean return — job done or poll cancelled (`Ok(())`).
  Done,
  /// Transient network failure — the same JIT config is still valid.
  Net,
  /// Transient protocol failure — the same JIT config is still valid.
  Proto,
  /// Fatal auth failure — the JIT config is dead and must be re-minted.
  Auth,
  /// Session conflict that outlasted the listener's bounded retry.
  Conflict,
}

/// Resolve a row's outcome marker to the listener result it stands for.
fn resolve(outcome: &Outcome) -> Result<(), RunnerError> {
  match outcome {
    Outcome::Done => Ok(()),
    Outcome::Net => Err(RunnerError::Network("timeout".into())),
    Outcome::Proto => Err(RunnerError::Protocol("unexpected shape".into())),
    Outcome::Auth => Err(RunnerError::Auth("401 unauthorized".into())),
    Outcome::Conflict => Err(RunnerError::SessionConflict(
      "create session returned 409 Conflict".into(),
    )),
  }
}

/// One decision-table row:
/// `(name, outcome, once, cancelled, pending_remove, expected)`.
type Row = (&'static str, Outcome, bool, bool, bool, LoopAction);

/// Rows that terminate the loop (rules 1-3: cancelled, once, `pending_remove`),
/// including the `cancelled` > `once` > `pending_remove` precedence rows.
fn exiting_cases() -> Vec<Row> {
  use LoopAction::Exit;
  use Outcome::{Conflict, Done, Net, Proto};
  vec![
    // Rule 1: cancelled wins over once + pending_remove + an error outcome.
    (
      "cancel beats once+pending+err",
      Net,
      true,
      true,
      true,
      Exit(0),
    ),
    ("cancel on a clean job", Done, false, true, false, Exit(0)),
    // Rule 2: once propagates the listener result as the exit code.
    ("once ok exits 0", Done, true, false, false, Exit(0)),
    ("once err exits 1", Proto, true, false, false, Exit(1)),
    // Rule 2 beats rule 3: once wins over pending_remove.
    (
      "once beats pending_remove",
      Done,
      true,
      false,
      true,
      Exit(0),
    ),
    // Rule 3: pending_remove exits cleanly (not cancelled, not once).
    ("pending_remove exits 0", Done, false, false, true, Exit(0)),
    // A spent session-conflict retry exits 5, with or without --once, and
    // never falls through to the generic backoff (#91).
    (
      "once conflict exits 5",
      Conflict,
      true,
      false,
      false,
      Exit(5),
    ),
    (
      "loop conflict exits 5",
      Conflict,
      false,
      false,
      false,
      Exit(5),
    ),
    // Cancel and pending_remove still win: a deliberate stop is a clean exit.
    (
      "cancel beats conflict",
      Conflict,
      false,
      true,
      false,
      Exit(0),
    ),
    (
      "pending_remove beats conflict",
      Conflict,
      false,
      false,
      true,
      Exit(0),
    ),
  ]
}

/// Rows that keep the loop going (rules 4-6: re-register on a spent or
/// auth-dead JIT config, back off on a transient failure).
fn continuing_cases() -> Vec<Row> {
  use LoopAction::{BackoffRetry, Reregister};
  use Outcome::{Auth, Done, Net, Proto};
  vec![
    // Rule 4: a completed job re-registers (JIT config spent).
    (
      "job done re-registers",
      Done,
      false,
      false,
      false,
      Reregister,
    ),
    // Rule 5: an auth failure re-registers (JIT config dead).
    (
      "auth error re-registers",
      Auth,
      false,
      false,
      false,
      Reregister,
    ),
    // Rule 6: transient failures back off and retry the same JIT config.
    (
      "network error backs off",
      Net,
      false,
      false,
      false,
      BackoffRetry,
    ),
    (
      "protocol error backs off",
      Proto,
      false,
      false,
      false,
      BackoffRetry,
    ),
  ]
}

#[test]
fn next_action_covers_every_branch() {
  let mut rows = exiting_cases();
  rows.extend(continuing_cases());
  for (name, outcome, once, cancelled, pending_remove, expected) in rows {
    let got = next_action(&resolve(&outcome), once, cancelled, pending_remove);
    assert_eq!(got, expected, "case: {name}");
  }
}

/// One ephemeral decision-table row:
/// `(name, outcome, once, cancelled, pending_remove, expected)`.
type EphemeralRow = (
  &'static str,
  Result<Option<Conclusion>, RunnerError>,
  bool,
  bool,
  bool,
  EphemeralAction,
);

/// The ephemeral table (#92): a job that ran always unregisters; otherwise
/// cancel > `--once` > `pending_remove` > conflict > auth > retry.
fn ephemeral_cases() -> Vec<EphemeralRow> {
  use EphemeralAction::{BackoffRetry, Exit, Unregister};
  let conflict = || RunnerError::SessionConflict("409".into());
  vec![
    (
      "success job",
      Ok(Some(Conclusion::Success)),
      false,
      false,
      false,
      Unregister,
    ),
    (
      "failed job",
      Ok(Some(Conclusion::Failure)),
      false,
      false,
      false,
      Unregister,
    ),
    (
      "cancelled job + cancel",
      Ok(Some(Conclusion::Cancelled)),
      false,
      true,
      false,
      Unregister,
    ),
    (
      "job + once + pending",
      Ok(Some(Conclusion::Success)),
      true,
      false,
      true,
      Unregister,
    ),
    ("cancel before a job", Ok(None), false, true, false, Exit(0)),
    (
      "cancel beats conflict",
      Err(conflict()),
      true,
      true,
      true,
      Exit(0),
    ),
    ("once, no job", Ok(None), true, false, false, Exit(0)),
    (
      "once + conflict",
      Err(conflict()),
      true,
      false,
      false,
      Exit(5),
    ),
    (
      "once + network",
      Err(RunnerError::Network("reset".into())),
      true,
      false,
      false,
      Exit(1),
    ),
    (
      "once beats pending",
      Err(RunnerError::Auth("401".into())),
      true,
      false,
      true,
      Exit(1),
    ),
    ("pending, no job", Ok(None), false, false, true, Exit(0)),
    (
      "pending beats conflict",
      Err(conflict()),
      false,
      false,
      true,
      Exit(0),
    ),
    ("conflict", Err(conflict()), false, false, false, Exit(5)),
    (
      "dead JIT",
      Err(RunnerError::Auth("401".into())),
      false,
      false,
      false,
      Exit(1),
    ),
    (
      "no job, not cancelled",
      Ok(None),
      false,
      false,
      false,
      BackoffRetry,
    ),
    (
      "network",
      Err(RunnerError::Network("timeout".into())),
      false,
      false,
      false,
      BackoffRetry,
    ),
    (
      "protocol",
      Err(RunnerError::Protocol("shape".into())),
      false,
      false,
      false,
      BackoffRetry,
    ),
  ]
}

/// AC-8: every row of the spec's ephemeral decision table.
#[test]
fn ephemeral_decision_table() {
  for (name, outcome, once, cancelled, pending, expected) in ephemeral_cases() {
    assert_eq!(
      next_ephemeral_action(&outcome, once, cancelled, pending),
      expected,
      "row: {name}"
    );
  }
}
