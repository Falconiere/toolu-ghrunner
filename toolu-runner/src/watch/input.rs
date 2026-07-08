//! Keyboard mapping for the watch TUI: key events → high-level `Action`s,
//! including the cancel-confirm modal that swallows every key until
//! answered.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::state::App;

/// High-level command produced by one key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
  /// Exit the TUI.
  Quit,
  /// Move the focused selection up / down.
  MoveUp,
  MoveDown,
  /// Open the selected job in the detail pane.
  OpenSelected,
  /// Switch focus between the jobs and detail panes.
  TogglePane,
  /// Toggle log follow (auto-scroll) mode.
  ToggleFollow,
  /// Scroll the log pane by a page.
  PageUp,
  PageDown,
  /// `c`: ask for cancel confirmation.
  RequestCancel,
  /// `y` while confirming: deliver SIGINT to the runner.
  ConfirmCancel,
  /// Any other key while confirming: dismiss the prompt.
  DismissCancel,
  /// Key with no binding.
  None,
}

/// Map a key event to an `Action`, honoring the confirm modal.
pub fn action_for(app: &App, key: KeyEvent) -> Action {
  if app.confirm_cancel {
    return if matches!(key.code, KeyCode::Char('y' | 'Y')) {
      Action::ConfirmCancel
    } else {
      Action::DismissCancel
    };
  }
  if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
    return Action::Quit;
  }
  key_binding(key.code)
}

/// The static (non-modal) key bindings. An if-chain rather than a match:
/// `KeyCode` is non-exhaustive and the lint set forbids wildcard arms.
fn key_binding(code: KeyCode) -> Action {
  if matches!(code, KeyCode::Char('q') | KeyCode::Esc) {
    return Action::Quit;
  }
  if code == KeyCode::Char('c') {
    return Action::RequestCancel;
  }
  if code == KeyCode::Char('f') {
    return Action::ToggleFollow;
  }
  if code == KeyCode::Tab {
    return Action::TogglePane;
  }
  if matches!(code, KeyCode::Up | KeyCode::Char('k')) {
    return Action::MoveUp;
  }
  if matches!(code, KeyCode::Down | KeyCode::Char('j')) {
    return Action::MoveDown;
  }
  if code == KeyCode::Enter {
    return Action::OpenSelected;
  }
  if code == KeyCode::PageUp {
    return Action::PageUp;
  }
  if code == KeyCode::PageDown {
    return Action::PageDown;
  }
  Action::None
}
