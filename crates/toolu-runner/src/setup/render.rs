//! Legend output (epic #199): a rail and one accent for what needs the
//! user, `✓` done (muted, never green), `▲` warning, `✕` error, a `●`
//! waiting line (no spinner). With no TTY or `NO_COLOR`, one plain
//! `setup: <step> <result>` line per step and no rail or color.

/// Step renderer: interactive (rail) or plain (screen 15).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Out {
  /// Plain `setup:` lines, no rail/color.
  pub(crate) plain: bool,
}

impl Out {
  /// A completed step.
  pub(crate) fn done(self, step: &str, result: &str) {
    if self.plain {
      println!("setup: {step} {result}");
    } else {
      println!("✓ {step}: {result}");
    }
  }

  /// A warning step (continues).
  pub(crate) fn warn(self, step: &str, result: &str) {
    if self.plain {
      println!("setup: {step} warning: {result}");
    } else {
      println!("▲ {step}: {result}");
    }
  }

  /// A failed step.
  pub(crate) fn error(self, step: &str, result: &str) {
    if self.plain {
      println!("setup: {step} error: {result}");
    } else {
      println!("✕ {step}: {result}");
    }
  }

  /// A waiting line (blinking marker in a terminal; static plain text).
  pub(crate) fn waiting(self, step: &str, result: &str) {
    if self.plain {
      println!("setup: {step} waiting: {result}");
    } else {
      println!("● {step}: {result}");
    }
  }

  /// The rail opening for a section that will ask the user something.
  pub(crate) fn rail(self, title: &str) {
    if !self.plain {
      println!("┌ {title}");
    }
  }

  /// A rail body line.
  pub(crate) fn line(self, text: &str) {
    if self.plain {
      println!("{text}");
    } else {
      println!("│ {text}");
    }
  }

  /// The rail closing line.
  pub(crate) fn end(self, text: &str) {
    if self.plain {
      println!("{text}");
    } else {
      println!("└ {text}");
    }
  }
}

/// Whether plain output applies: no stdout TTY, or `NO_COLOR` set.
/// The `no_color` value is read by the caller through the crate's
/// `config` module (the only env reader).
pub(crate) fn plain_mode(stdout_is_tty: bool, no_color: bool) -> bool {
  !stdout_is_tty || no_color
}

#[cfg(test)]
#[path = "tests/render.rs"]
mod tests;
