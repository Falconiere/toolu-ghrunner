//! `wizard::input` key mapping (AC-4): the v1 driver acts only on quit, so
//! `q`, `Ctrl-C`, and `Esc` map to `Quit` and everything else maps to
//! `None`. Real `KeyEvent`s, no mocks.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use observability::wizard::input::{Action, action_for};
use observability::wizard::state::{SetupInputs, WizardState};

/// A key press with no modifiers.
fn key(code: KeyCode) -> KeyEvent {
  KeyEvent::new(code, KeyModifiers::NONE)
}

/// Fresh wizard state for binding assertions.
fn fresh() -> WizardState {
  WizardState::new(SetupInputs::default())
}

#[test]
fn quit_keys_map_to_quit() {
  let state = fresh();
  assert_eq!(action_for(&state, key(KeyCode::Char('q'))), Action::Quit);
  assert_eq!(action_for(&state, key(KeyCode::Esc)), Action::Quit);
  let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
  assert_eq!(action_for(&state, ctrl_c), Action::Quit);
}

#[test]
fn other_keys_map_to_none() {
  let state = fresh();
  let cases = [
    key(KeyCode::Up),
    key(KeyCode::Down),
    key(KeyCode::Enter),
    key(KeyCode::Backspace),
    key(KeyCode::Char('a')),
    key(KeyCode::Char('r')),
    // A bare 'c' (no CONTROL) is not a quit.
    key(KeyCode::Char('c')),
  ];
  for k in cases {
    assert_eq!(action_for(&state, k), Action::None, "binding for {k:?}");
  }
}
