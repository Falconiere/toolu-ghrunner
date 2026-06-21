use std::collections::HashMap;

/// A parsed GitHub Actions workflow command from stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowCommand {
  Error {
    message: String,
    file: Option<String>,
    line: Option<u32>,
    col: Option<u32>,
    end_line: Option<u32>,
    end_column: Option<u32>,
    title: Option<String>,
  },
  Warning {
    message: String,
    file: Option<String>,
    line: Option<u32>,
    col: Option<u32>,
    end_line: Option<u32>,
    end_column: Option<u32>,
    title: Option<String>,
  },
  Notice {
    message: String,
    file: Option<String>,
    line: Option<u32>,
    col: Option<u32>,
    end_line: Option<u32>,
    end_column: Option<u32>,
    title: Option<String>,
  },
  Debug {
    message: String,
  },
  Group {
    title: String,
  },
  EndGroup,
  SetOutput {
    name: String,
    value: String,
  },
  AddMask {
    value: String,
  },
  SaveState {
    name: String,
    value: String,
  },
  AddPath {
    value: String,
  },
  SetEnv {
    name: String,
    value: String,
  },
  Echo {
    on: bool,
  },
  StopCommands {
    token: String,
  },
  ResumeCommands {
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
