//! Matcher command effects and masked diagnostic emission for process output.

use super::super::context::ExecutionContext;
use super::super::problem_matcher_path::{annotation_path, read_matcher};
use super::{CommandDispatcher, LineDisposition, unescape_data, unescape_property};
use shared::{AnnotationLevel, LogStream, RunnerEvent};

impl CommandDispatcher {
  pub(super) fn add_matcher(&mut self, path: &str, ctx: &mut ExecutionContext) {
    let path = unescape_data(path);
    if path.is_empty() {
      self.matcher_diagnostic(AnnotationLevel::Warning, "add-matcher requires a file path");
      return;
    }
    let result = read_matcher(&path, ctx).and_then(|text| ctx.matchers.add_json(&text));
    if let Err(error) = result {
      self.matcher_failure(&error);
    }
  }

  pub(super) fn remove_matcher(
    &mut self,
    owner: Option<&str>,
    path: &str,
    ctx: &mut ExecutionContext,
  ) {
    let path = unescape_data(path);
    let owner = owner
      .map(unescape_property)
      .filter(|owner| !owner.is_empty());
    match (owner, path.is_empty()) {
      (Some(owner), true) => ctx.matchers.remove_owner(&owner),
      (None, false) => {
        let result = read_matcher(&path, ctx).and_then(|text| ctx.matchers.remove_json(&text));
        if let Err(error) = result {
          self.matcher_failure(&error);
        }
      },
      _ => self.matcher_diagnostic(
        AnnotationLevel::Warning,
        "remove-matcher requires either owner or a file path, not both",
      ),
    }
  }

  fn matcher_failure(&mut self, message: &str) {
    self.failed = true;
    self.matcher_diagnostic(AnnotationLevel::Error, message);
  }

  fn matcher_diagnostic(&mut self, level: AnnotationLevel, message: &str) {
    self.pending.push(RunnerEvent::Annotation {
      step_id: self.log_step_id.clone(),
      level,
      message: self.mask(message),
      file: None,
      line: None,
      col: None,
      end_line: None,
      end_column: None,
      title: None,
    });
  }

  pub(super) fn match_output(&mut self, text: &str, ctx: &ExecutionContext) -> LineDisposition {
    let state = match self.output_stream {
      LogStream::Stdout => &mut self.matcher_stdout,
      LogStream::Stderr => &mut self.matcher_stderr,
    };
    let result = state.match_line(&ctx.matchers, text);
    if text.len() > 64 * 1024 && !self.long_line_warned && !ctx.matchers.definitions().is_empty() {
      self.long_line_warned = true;
      self.matcher_diagnostic(
        AnnotationLevel::Warning,
        "problem matching skipped output over 64 KiB; original log retained",
      );
    }
    if let Some(result) = result {
      let level = match result
        .severity
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
      {
        "warning" => AnnotationLevel::Warning,
        "notice" => AnnotationLevel::Notice,
        _ => AnnotationLevel::Error,
      };
      let message = self.mask(result.message.as_deref().unwrap_or_default());
      let file = annotation_path(result.file.as_deref(), result.from_path.as_deref(), ctx)
        .map(|file| self.mask(&file));
      let line = coordinate(result.line.as_deref());
      let col = coordinate(result.column.as_deref());
      self.pending.push(RunnerEvent::Annotation {
        step_id: self.log_step_id.clone(),
        level,
        message,
        file,
        line,
        col,
        end_line: line,
        end_column: col,
        title: None,
      });
    }
    LineDisposition::PassThrough(text.to_owned())
  }
}

fn coordinate(value: Option<&str>) -> Option<i32> {
  let value =
    value.filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))?;
  value.parse().ok()
}
