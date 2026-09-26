//! Shell selection, script preparation and argument substitution shared by handlers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use shared::RunnerError;

/// A validated shell command; substitution happens after argument tokenization.
pub(super) struct ShellCommand {
  /// Host executable path or a command resolved inside the job container.
  pub(super) program: PathBuf,
  name: String,
  arguments: Vec<String>,
  builtin_pwsh: bool,
}

impl ShellCommand {
  /// Resolve explicit or default shell semantics without spawning a process.
  pub(super) async fn resolve(
    shell: Option<&str>,
    env: &HashMap<String, String>,
    working_dir: &Path,
    container: bool,
  ) -> Result<Self, RunnerError> {
    let (name, arguments, builtin_pwsh) = match shell.filter(|value| !value.is_empty()) {
      None => {
        let name = if !container && find_executable("bash", env, working_dir).await.is_some() {
          "bash"
        } else {
          "sh"
        };
        (
          name.to_owned(),
          vec!["-e".to_owned(), "{0}".to_owned()],
          false,
        )
      },
      Some(shell) => parse_shell(shell)?,
    };
    let program = if container {
      PathBuf::from(&name)
    } else {
      find_executable(&name, env, working_dir)
        .await
        .ok_or_else(|| {
          RunnerError::ScriptHandler(format!(
            "shell executable '{name}' was not found or is not executable (cwd: {})",
            working_dir.display()
          ))
        })?
    };
    Ok(Self {
      program,
      name,
      arguments,
      builtin_pwsh,
    })
  }

  /// Produce argv while keeping substituted paths inside their original argument.
  pub(super) fn args(&self, path: &str) -> Result<Vec<String>, RunnerError> {
    let path = if self.builtin_pwsh {
      path.replace('\'', "''")
    } else {
      path.to_owned()
    };
    self
      .arguments
      .iter()
      .map(|arg| format_argument(arg, &path).map(|(value, _)| value))
      .collect()
  }

  /// Write the upstream extension and interpreter-specific prologue/epilogue.
  pub(super) fn write_script(
    &self,
    script: &str,
    directory: Option<&Path>,
  ) -> Result<tempfile::NamedTempFile, RunnerError> {
    let suffix = match self.name.to_ascii_lowercase().as_str() {
      "bash" | "sh" => ".sh",
      "python" => ".py",
      "pwsh" => ".ps1",
      _ => "",
    };
    let mut builder = tempfile::Builder::new();
    builder.suffix(suffix);
    let mut file = match directory {
      Some(directory) => builder.tempfile_in(directory),
      None => builder.tempfile(),
    }
    .map_err(|error| RunnerError::ScriptHandler(format!("temp script: {error}")))?;
    let contents = if self.name.eq_ignore_ascii_case("pwsh") {
      format!(
        "$ErrorActionPreference = 'stop'\n{script}\nif ((Test-Path -LiteralPath variable:\\LASTEXITCODE)) {{ exit $LASTEXITCODE }}"
      )
    } else {
      script.to_owned()
    };
    std::io::Write::write_all(&mut file, contents.as_bytes())
      .map_err(|error| RunnerError::ScriptHandler(format!("write script: {error}")))?;
    Ok(file)
  }
}

fn invalid(reason: &str) -> RunnerError {
  RunnerError::ScriptHandler(format!(
    "invalid shell option: {reason}; use bash, sh, python, pwsh or a command template containing '{{0}}'"
  ))
}

fn parse_shell(shell: &str) -> Result<(String, Vec<String>, bool), RunnerError> {
  if shell.contains('\0') {
    return Err(invalid("NUL is not permitted"));
  }
  let words = shlex::split(shell).ok_or_else(|| invalid("unterminated quote or escape"))?;
  let mut words = words.into_iter();
  let name = words
    .next()
    .filter(|name| !name.is_empty())
    .ok_or_else(|| invalid("empty executable"))?;
  let mut arguments: Vec<String> = words.collect();
  let builtin_pwsh = arguments.is_empty() && name.eq_ignore_ascii_case("pwsh");
  if arguments.is_empty() {
    let template = match name.to_ascii_lowercase().as_str() {
      "bash" => "--noprofile --norc -e -o pipefail {0}",
      "sh" => "-e {0}",
      "python" => "{0}",
      "pwsh" => "-command \". '{0}'\"",
      _ => return Err(invalid("unknown shell without a template")),
    };
    arguments = shlex::split(template).ok_or_else(|| invalid("invalid built-in template"))?;
  }
  let mut has_path = false;
  for argument in &arguments {
    has_path |= format_argument(argument, "")?.1;
  }
  if !has_path {
    return Err(invalid("template is missing {0}"));
  }
  Ok((name, arguments, builtin_pwsh))
}

/// Interpret only the script field and escaped literal braces, never shell code.
fn format_argument(template: &str, path: &str) -> Result<(String, bool), RunnerError> {
  let mut result = String::new();
  let mut chars = template.chars();
  let mut has_path = false;
  while let Some(ch) = chars.next() {
    match ch {
      '{' => match chars.next() {
        Some('{') => result.push('{'),
        Some('0') if chars.next() == Some('}') => {
          result.push_str(path);
          has_path = true;
        },
        _ => return Err(invalid("unsupported or unmatched format field")),
      },
      '}' => match chars.next() {
        Some('}') => result.push('}'),
        _ => return Err(invalid("unmatched closing brace")),
      },
      _ => result.push(ch),
    }
  }
  Ok((result, has_path))
}

async fn find_executable(name: &str, env: &HashMap<String, String>, cwd: &Path) -> Option<PathBuf> {
  if name.contains('/') {
    let path = cwd.join(name);
    return executable(&path).await.then_some(path);
  }
  let Some(path) = env.get("PATH").cloned().or_else(crate::config::path) else {
    tracing::warn!(
      shell = name,
      "shell executable lookup skipped because PATH is unavailable"
    );
    return None;
  };
  for candidate in std::env::split_paths(&path).map(|directory| cwd.join(directory).join(name)) {
    if executable(&candidate).await {
      return Some(candidate);
    }
  }
  None
}

async fn executable(path: &Path) -> bool {
  let Ok(metadata) = tokio::fs::metadata(path).await else {
    return false;
  };
  if !metadata.is_file() {
    return false;
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
  }
  #[cfg(not(unix))]
  {
    true
  }
}
