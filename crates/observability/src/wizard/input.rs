//! Keyboard mapping for the setup wizard: key events → high-level
//! `Action`s. Pure — an if-chain rather than a `match`, since `KeyCode` is
//! non-exhaustive and the lint set forbids wildcard match arms. The v1
//! driver is progress-display only (no in-TUI editing, no retry), so the
//! surface is just quit vs. ignore.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::state::WizardState;

/// High-level command produced by one key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
  /// Exit the wizard.
  Quit,
  /// Key with no binding.
  None,
}

/// Map a key event to an `Action`. `q`, `Ctrl-C`, and `Esc` quit; every
/// other key is ignored (the v1 driver has no editing or retry surface).
/// `_state` is unused today but kept in the signature so the driver's call
/// site is stable if later versions gate keys on wizard state.
pub fn action_for(_state: &WizardState, key: KeyEvent) -> Action {
  let code = key.code;
  if code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
    return Action::Quit;
  }
  if code == KeyCode::Char('q') || code == KeyCode::Esc {
    return Action::Quit;
  }
  Action::None
}
