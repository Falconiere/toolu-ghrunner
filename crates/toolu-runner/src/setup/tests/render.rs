//! Plain-mode rule: no TTY or `NO_COLOR` means screen-15 lines.

use super::plain_mode;

#[test]
fn plain_when_no_tty_or_no_color() {
  assert!(plain_mode(false, false));
  assert!(plain_mode(true, true));
  assert!(!plain_mode(true, false));
}
