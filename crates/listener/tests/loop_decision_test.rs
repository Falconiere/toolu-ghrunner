//! Table-driven tests for `listener::loop_decision::next_action` (AC-4).
//!
//! Real data only: every case resolves to a real `RunnerError` variant and
//! exercises one branch of the normative decision order, including the
//! precedence between `cancelled`, `once`, and `pending_remove`.

use listener::loop_decision::{LoopAction, next_action};
use shared::RunnerError;

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
}

/// Resolve a row's outcome marker to the listener result it stands for.
fn resolve(outcome: &Outcome) -> Result<(), RunnerError> {
  match outcome {
    Outcome::Done => Ok(()),
    Outcome::Net => Err(RunnerError::Network("timeout".into())),
    Outcome::Proto => Err(RunnerError::Protocol("unexpected shape".into())),
    Outcome::Auth => Err(RunnerError::Auth("401 unauthorized".into())),
  }
}

/// One decision-table row:
/// `(name, outcome, once, cancelled, pending_remove, expected)`.
type Row = (&'static str, Outcome, bool, bool, bool, LoopAction);

/// Rows that terminate the loop (rules 1-3: cancelled, once, pending_remove),
/// including the `cancelled` > `once` > `pending_remove` precedence rows.
fn exiting_cases() -> Vec<Row> {
  use LoopAction::Exit;
  use Outcome::{Done, Net, Proto};
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
