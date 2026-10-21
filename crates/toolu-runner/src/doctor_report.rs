//! `doctor` report model: leveled rows grouped into titled sections, the
//! masked renderer and the aggregate exit code.
//!
//! Exit codes: `0` no `FAIL` row (warnings allowed), `1` at least one. Usage
//! errors (`2`) are decided before a report exists. A section that embeds
//! `check`'s rendered lines carries the failures and warnings among them.

use shared::SecretMasker;

/// Exit code when at least one row failed.
pub(crate) const EXIT_FAILED: i32 = 1;

/// How a row turned out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Level {
  /// Healthy.
  Ok,
  /// A fact worth showing; never a problem.
  Info,
  /// Not checked (offline, not applicable, or could not be determined
  /// without that being a problem).
  Skip,
  /// Degraded or risky, but jobs still run.
  Warn,
  /// Jobs will not run (or will stop) for something this machine has.
  Fail,
}

impl Level {
  /// The fixed-width tag, matching `check`'s vocabulary.
  fn tag(self) -> &'static str {
    match self {
      Self::Ok => "OK  ",
      Self::Info => "INFO",
      Self::Skip => "SKIP",
      Self::Warn => "WARN",
      Self::Fail => "FAIL",
    }
  }
}

/// One finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
  /// Its level.
  pub(crate) level: Level,
  /// Short label (`token`, `run`, `disk`, …).
  pub(crate) name: String,
  /// What was found.
  pub(crate) detail: String,
  /// The command or action that fixes it, printed on its own line.
  pub(crate) fix: Option<String>,
}

impl Row {
  /// A row without a fix line.
  pub(crate) fn new(level: Level, name: &str, detail: impl Into<String>) -> Self {
    Self {
      level,
      name: name.to_owned(),
      detail: detail.into(),
      fix: None,
    }
  }

  /// Attach the fix line.
  pub(crate) fn with_fix(mut self, fix: impl Into<String>) -> Self {
    self.fix = Some(fix.into());
    self
  }
}

/// A line inside a section.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Line {
  /// A leveled finding.
  Row(Row),
  /// Printed as-is (sub-headings, `check`'s rendered lines).
  Text(String),
}

/// A titled group of lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Section {
  title: String,
  lines: Vec<Line>,
  /// Failures among the `Text` lines, counted by their producer.
  text_failures: usize,
  /// Warnings among the `Text` lines.
  text_warnings: usize,
}

impl Section {
  /// A section of rows.
  pub(crate) fn new(title: &str, rows: Vec<Row>) -> Self {
    Self {
      title: title.to_owned(),
      lines: rows.into_iter().map(Line::Row).collect(),
      text_failures: 0,
      text_warnings: 0,
    }
  }

  /// A section of pre-rendered lines whose producer counted the failures
  /// and warnings among them.
  pub(crate) fn verbatim(
    title: &str,
    lines: Vec<String>,
    failures: usize,
    warnings: usize,
  ) -> Self {
    Self {
      title: title.to_owned(),
      lines: lines.into_iter().map(Line::Text).collect(),
      text_failures: failures,
      text_warnings: warnings,
    }
  }

  /// Append a sub-heading line (e.g. one registration's title).
  pub(crate) fn push_heading(&mut self, heading: String) {
    self.lines.push(Line::Text(heading));
  }

  /// Append rows.
  pub(crate) fn extend(&mut self, rows: impl IntoIterator<Item = Row>) {
    self.lines.extend(rows.into_iter().map(Line::Row));
  }

  fn count(&self, level: Level) -> usize {
    self
      .lines
      .iter()
      .filter(|line| matches!(line, Line::Row(row) if row.level == level))
      .count()
  }

  fn failures(&self) -> usize {
    self.count(Level::Fail) + self.text_failures
  }

  fn warnings(&self) -> usize {
    self.count(Level::Warn) + self.text_warnings
  }
}

/// `0` with no failure anywhere, else [`EXIT_FAILED`].
pub(crate) fn exit_code(sections: &[Section]) -> i32 {
  if sections.iter().map(Section::failures).sum::<usize>() == 0 {
    0
  } else {
    EXIT_FAILED
  }
}

fn plural(count: usize, noun: &str) -> String {
  if count == 1 {
    format!("{count} {noun}")
  } else {
    format!("{count} {noun}s")
  }
}

fn row_lines(row: &Row, out: &mut Vec<String>) {
  out.push(format!(
    "  {}  {:<10} {}",
    row.level.tag(),
    row.name,
    row.detail
  ));
  if let Some(fix) = &row.fix {
    out.push(format!("        fix: {fix}"));
  }
}

/// The full report for `host`; every line passes through `masker`.
pub(crate) fn render(host: &str, sections: &[Section], masker: &SecretMasker) -> String {
  let mut out = vec![format!("toolu-runner doctor — {host}")];
  for section in sections {
    out.push(format!("{}:", section.title));
    for line in &section.lines {
      match line {
        Line::Row(row) => row_lines(row, &mut out),
        Line::Text(text) => out.push(text.clone()),
      }
    }
  }
  let failures: usize = sections.iter().map(Section::failures).sum();
  let warnings: usize = sections.iter().map(Section::warnings).sum();
  out.push(format!(
    "result: {}, {} (exit {})",
    plural(failures, "failure"),
    plural(warnings, "warning"),
    exit_code(sections)
  ));
  let mut text = out
    .iter()
    .map(|line| masker.mask(line).into_owned())
    .collect::<Vec<_>>()
    .join("\n");
  text.push('\n');
  text
}

#[cfg(test)]
#[path = "tests/doctor_report.rs"]
mod tests;
