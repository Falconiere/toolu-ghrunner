use std::collections::HashMap;

/// A parsed GitHub Actions workflow command from stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowCommand {
  /// `::error::` — an error annotation on the job.
  Error {
    /// Annotation message text.
    message: String,
    /// File the annotation points at, if any.
    file: Option<String>,
    /// Start line, if any.
    line: Option<u32>,
    /// Start column, if any.
    col: Option<u32>,
    /// End line, if any.
    end_line: Option<u32>,
    /// End column, if any.
    end_column: Option<u32>,
    /// Annotation title, if any.
    title: Option<String>,
  },
  /// `::warning::` — a warning annotation on the job.
  Warning {
    /// Annotation message text.
    message: String,
    /// File the annotation points at, if any.
    file: Option<String>,
    /// Start line, if any.
    line: Option<u32>,
    /// Start column, if any.
    col: Option<u32>,
    /// End line, if any.
    end_line: Option<u32>,
    /// End column, if any.
    end_column: Option<u32>,
    /// Annotation title, if any.
    title: Option<String>,
  },
  /// `::notice::` — an informational annotation on the job.
  Notice {
    /// Annotation message text.
    message: String,
    /// File the annotation points at, if any.
    file: Option<String>,
    /// Start line, if any.
    line: Option<u32>,
    /// Start column, if any.
    col: Option<u32>,
    /// End line, if any.
    end_line: Option<u32>,
    /// End column, if any.
    end_column: Option<u32>,
    /// Annotation title, if any.
    title: Option<String>,
  },
  /// `::debug::` — a debug-level log line.
  Debug {
    /// Debug message text.
    message: String,
  },
  /// `::group::` — starts a collapsible log group.
  Group {
    /// Group title shown in the GitHub UI.
    title: String,
  },
  /// `::endgroup::` — closes the current log group.
  EndGroup,
  /// `::set-output::` — sets a step output (legacy; superseded by `GITHUB_OUTPUT`).
  SetOutput {
    /// Output name.
    name: String,
    /// Output value.
    value: String,
  },
  /// `::add-mask::` — registers a value to be masked in subsequent logs.
  AddMask {
    /// Value to mask.
    value: String,
  },
  /// `::save-state::` — saves state for the action's post step (legacy; superseded by `GITHUB_STATE`).
  SaveState {
    /// State key.
    name: String,
    /// State value.
    value: String,
  },
  /// `::add-path::` — prepends a directory to `PATH` for later steps (legacy; superseded by `GITHUB_PATH`).
  AddPath {
    /// Directory to prepend.
    value: String,
  },
  /// `::set-env::` — sets an env var for later steps (legacy; superseded by `GITHUB_ENV`).
  SetEnv {
    /// Environment variable name.
    name: String,
    /// Environment variable value.
    value: String,
  },
  /// `::echo::` — toggles command-echoing to the log.
  Echo {
    /// Whether echoing is being turned on.
    on: bool,
  },
  /// `::stop-commands::` — suspends workflow-command processing until the matching token.
  StopCommands {
    /// Token that must appear on its own to resume processing.
    token: String,
  },
  /// Resumes workflow-command processing (matches a prior `StopCommands` token).
  ResumeCommands {
    /// Token that matched the pending `StopCommands`.
    token: String,
  },
}

/// Parse a stdout line for GitHub Actions workflow commands.
///
/// Returns `None` if the line is not a command (doesn't start with `::`).
pub fn parse_command(line: &str) -> Option<WorkflowCommand> {
  let rest = line.strip_prefix("::")?;
  let (head, value) = rest.split_once("::")?;

  let (command, props) = split_command_and_props(head);

  match command {
    "error" => Some(build_annotation(value, &props, AnnotationKind::Error)),
    "warning" => Some(build_annotation(value, &props, AnnotationKind::Warning)),
    "notice" => Some(build_annotation(value, &props, AnnotationKind::Notice)),
    "debug" => Some(WorkflowCommand::Debug {
      message: value.to_owned(),
    }),
    "group" => Some(WorkflowCommand::Group {
      title: value.to_owned(),
    }),
    "endgroup" => Some(WorkflowCommand::EndGroup),
    "set-output" => Some(WorkflowCommand::SetOutput {
      name: props.get("name").cloned().unwrap_or_default(),
      value: value.to_owned(),
    }),
    "add-mask" => Some(WorkflowCommand::AddMask {
      value: value.to_owned(),
    }),
    "save-state" => Some(WorkflowCommand::SaveState {
      name: props.get("name").cloned().unwrap_or_default(),
      value: value.to_owned(),
    }),
    "add-path" => Some(WorkflowCommand::AddPath {
      value: value.to_owned(),
    }),
    "set-env" => Some(WorkflowCommand::SetEnv {
      name: props.get("name").cloned().unwrap_or_default(),
      value: value.to_owned(),
    }),
    "echo" => Some(WorkflowCommand::Echo {
      on: value.trim().eq_ignore_ascii_case("on"),
    }),
    "stop-commands" => Some(WorkflowCommand::StopCommands {
      token: value.to_owned(),
    }),
    _ => None,
  }
}

fn split_command_and_props(head: &str) -> (&str, HashMap<String, String>) {
  if let Some(space_pos) = head.find(' ') {
    let command = head.get(..space_pos).unwrap_or_default();
    let props_str = head.get(space_pos + 1..).unwrap_or_default();
    (command, parse_props(props_str))
  } else {
    (head, HashMap::new())
  }
}

fn parse_props(props_str: &str) -> HashMap<String, String> {
  let mut map = HashMap::new();
  for pair in props_str.split(',') {
    if let Some((key, value)) = pair.split_once('=') {
      map.insert(key.trim().to_owned(), value.trim().to_owned());
    }
  }
  map
}

#[derive(Clone, Copy)]
enum AnnotationKind {
  Error,
  Warning,
  Notice,
}

fn build_annotation(
  message: &str,
  props: &HashMap<String, String>,
  kind: AnnotationKind,
) -> WorkflowCommand {
  let file = props.get("file").cloned();
  let line = props.get("line").and_then(|v| v.parse().ok());
  let col = props.get("col").and_then(|v| v.parse().ok());
  let end_line = props.get("endLine").and_then(|v| v.parse().ok());
  let end_column = props.get("endColumn").and_then(|v| v.parse().ok());
  let title = props.get("title").cloned();
  let message = message.to_owned();

  match kind {
    AnnotationKind::Error => WorkflowCommand::Error {
      message,
      file,
      line,
      col,
      end_line,
      end_column,
      title,
    },
    AnnotationKind::Warning => WorkflowCommand::Warning {
      message,
      file,
      line,
      col,
      end_line,
      end_column,
      title,
    },
    AnnotationKind::Notice => WorkflowCommand::Notice {
      message,
      file,
      line,
      col,
      end_line,
      end_column,
      title,
    },
  }
}
