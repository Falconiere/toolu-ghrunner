//! Per-step environment, output, state, path, and summary file commands.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use shared::RunnerError;

async fn run_blocking_file_io<T>(
  operation: &'static str,
  task: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> Result<T, RunnerError>
where
  T: Send + 'static,
{
  Ok(
    tokio::task::spawn_blocking(task)
      .await
      .map_err(|e| RunnerError::FileCommand(format!("{operation} task failed: {e}")))??,
  )
}

/// Create the directory that owns per-step file-command files on the blocking
/// pool, keeping synchronous filesystem work off the async executor.
///
/// # Errors
///
/// Returns `RunnerError::Io` if directory creation fails, or
/// `RunnerError::FileCommand` if the blocking task cannot be joined.
pub(super) async fn create_file_command_dir(path: &Path) -> Result<(), RunnerError> {
  let path = path.to_path_buf();
  run_blocking_file_io("file-command directory creation", move || {
    std::fs::create_dir_all(path)
  })
  .await
}

/// Manages temp files for GitHub Actions file commands.
pub struct FileCommandManager {
  /// Path backing `GITHUB_ENV`.
  pub env_path: PathBuf,
  /// Path backing `GITHUB_OUTPUT`.
  pub output_path: PathBuf,
  /// Path backing `GITHUB_PATH`.
  pub path_path: PathBuf,
  /// Path backing `GITHUB_STATE`.
  pub state_path: PathBuf,
  /// Path backing `GITHUB_STEP_SUMMARY`.
  pub summary_path: PathBuf,
}

/// Results from processing file commands after a step.
pub struct FileCommandResults {
  /// Env vars set via `GITHUB_ENV`.
  pub env_vars: HashMap<String, String>,
  /// Outputs set via `GITHUB_OUTPUT`.
  pub outputs: HashMap<String, String>,
  /// Directories prepended via `GITHUB_PATH`.
  pub path_additions: Vec<String>,
  /// State saved via `GITHUB_STATE`, for the action's post step.
  pub state: HashMap<String, String>,
  /// Commands that could not be processed, in upstream order; each fails
  /// its step. Pairs read before the error are still in the maps above.
  pub failures: Vec<FileCommandFailure>,
  /// Whether `GITHUB_ENV` tried to set the blocked `NODE_OPTIONS`.
  pub blocked_env: bool,
}

/// One file command that could not be processed. Upstream reports it as two
/// errors and fails the step (`FileCommandManager.ProcessFiles`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCommandFailure {
  /// Upstream `ContextName`: `path`, `env`, `state` or `output`.
  pub command: &'static str,
  /// The parser or I/O message.
  pub message: String,
}

impl FileCommandFailure {
  /// Upstream's first error line for this command.
  pub fn summary(&self) -> String {
    format!(
      "Unable to process file command '{}' successfully.",
      self.command
    )
  }
}

/// Upstream's error for a `GITHUB_ENV` write of the blocked `NODE_OPTIONS`.
/// It is reported without failing the step.
pub const BLOCKED_ENV_MESSAGE: &str =
  "Can't store NODE_OPTIONS output parameter using '$GITHUB_ENV' command.";

impl FileCommandManager {
  /// Create temp files for all file commands.
  ///
  /// Returns the manager and a map of env var names to file paths. The five
  /// synchronous file creations run as one task on Tokio's blocking pool so
  /// this per-step operation does not stall an async executor thread.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Io` if file creation fails, or
  /// `RunnerError::FileCommand` if the blocking task cannot be joined.
  pub async fn create(temp_dir: &Path) -> Result<(Self, HashMap<String, String>), RunnerError> {
    let mgr = Self {
      env_path: temp_dir.join("github_env"),
      output_path: temp_dir.join("github_output"),
      path_path: temp_dir.join("github_path"),
      state_path: temp_dir.join("github_state"),
      summary_path: temp_dir.join("github_step_summary"),
    };

    let paths = [
      mgr.env_path.clone(),
      mgr.output_path.clone(),
      mgr.path_path.clone(),
      mgr.state_path.clone(),
      mgr.summary_path.clone(),
    ];
    run_blocking_file_io("file-command file creation", move || {
      for path in paths {
        std::fs::write(path, "")?;
      }
      Ok(())
    })
    .await?;

    let mut env_map = HashMap::new();
    env_map.insert(
      "GITHUB_ENV".to_owned(),
      mgr.env_path.to_string_lossy().into_owned(),
    );
    env_map.insert(
      "GITHUB_OUTPUT".to_owned(),
      mgr.output_path.to_string_lossy().into_owned(),
    );
    env_map.insert(
      "GITHUB_PATH".to_owned(),
      mgr.path_path.to_string_lossy().into_owned(),
    );
    env_map.insert(
      "GITHUB_STATE".to_owned(),
      mgr.state_path.to_string_lossy().into_owned(),
    );
    env_map.insert(
      "GITHUB_STEP_SUMMARY".to_owned(),
      mgr.summary_path.to_string_lossy().into_owned(),
    );

    Ok((mgr, env_map))
  }

  /// Read and parse the path, environment, state and output files after a
  /// step. The four synchronous reads run as one task on Tokio's blocking
  /// pool. Each command is processed on its own, as upstream does: a
  /// malformed or unreadable file lands in `failures` without discarding the
  /// other commands or the pairs read before the error.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::FileCommand` if the blocking task cannot be joined.
  pub async fn process(&self) -> Result<FileCommandResults, RunnerError> {
    let paths = [
      self.path_path.clone(),
      self.env_path.clone(),
      self.state_path.clone(),
      self.output_path.clone(),
    ];
    let [path_read, env_read, state_read, output_read] =
      run_blocking_file_io("file-command file read", move || {
        Ok(paths.map(|path| read_command_file(&path)))
      })
      .await?;

    let mut failures = Vec::new();
    let path_additions = match path_read {
      Ok(content) => parse_path_file(&content),
      Err(message) => {
        failures.push(FileCommandFailure {
          command: "path",
          message,
        });
        Vec::new()
      },
    };
    let mut env_vars = parse_kv_command("env", env_read, &mut failures);
    let blocked_env = strip_blocked_env(&mut env_vars);
    let state = parse_kv_command("state", state_read, &mut failures);
    let outputs = parse_kv_command("output", output_read, &mut failures);

    Ok(FileCommandResults {
      env_vars,
      outputs,
      path_additions,
      state,
      failures,
      blocked_env,
    })
  }

  /// Reset all file command files for the next step. The five synchronous
  /// writes run as one task on Tokio's blocking pool.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Io` if writing fails, or
  /// `RunnerError::FileCommand` if the blocking task cannot be joined.
  pub async fn reset(&self) -> Result<(), RunnerError> {
    let paths = [
      self.env_path.clone(),
      self.output_path.clone(),
      self.path_path.clone(),
      self.state_path.clone(),
      self.summary_path.clone(),
    ];
    run_blocking_file_io("file-command file reset", move || {
      for path in paths {
        std::fs::write(path, "")?;
      }
      Ok(())
    })
    .await
  }
}

/// Strip env keys the runner refuses to propagate from a `$GITHUB_ENV`
/// read-back and report whether one was present. Today that is
/// `NODE_OPTIONS` (case-insensitive), upstream's `SetEnvFileCommand` block
/// list: node children honor it (e.g. `--require`), so letting a `run:` step
/// set it would inject a preload into every later node action. Applied to
/// BOTH the top-level file-command read-back and the composite `$GITHUB_ENV`
/// read-back.
pub fn strip_blocked_env<S: ::std::hash::BuildHasher>(
  env: &mut HashMap<String, String, S>,
) -> bool {
  let before = env.len();
  env.retain(|k, _| !k.eq_ignore_ascii_case("NODE_OPTIONS"));
  env.len() != before
}

/// Parse a `GITHUB_PATH` file — one path per line.
pub fn parse_path_file(content: &str) -> Vec<String> {
  content
    .lines()
    .map(|l| l.trim_end_matches('\r'))
    .filter(|l| !l.is_empty())
    .map(ToOwned::to_owned)
    .collect()
}

/// Read one command file the way upstream's `File.ReadAllText` does: a
/// missing file is empty, a UTF-8 BOM is dropped and invalid UTF-8 is
/// replaced rather than rejected. Any other I/O error is the command's
/// failure message.
pub(crate) fn read_command_file(path: &Path) -> Result<String, String> {
  match std::fs::read(path) {
    Ok(bytes) => {
      let body = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
      Ok(String::from_utf8_lossy(body).into_owned())
    },
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
    Err(error) => Err(format!(
      "Unable to read file command file '{}': {error}",
      path.display()
    )),
  }
}

/// Parse one read `KEY=VALUE` command file, recording its failure (an I/O or
/// parser message) under `command` while keeping the pairs read before it.
pub(crate) fn parse_kv_command(
  command: &'static str,
  read: Result<String, String>,
  failures: &mut Vec<FileCommandFailure>,
) -> HashMap<String, String> {
  let (pairs, error) = match read {
    Ok(content) => {
      let file = parse_kv_file(&content);
      (file.pairs, file.error)
    },
    Err(message) => (HashMap::new(), Some(message)),
  };
  if let Some(message) = error {
    failures.push(FileCommandFailure { command, message });
  }
  pairs
}

/// A parsed `GITHUB_ENV` / `GITHUB_OUTPUT` / `GITHUB_STATE` file.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct KvFile {
  /// Pairs read before the first error; a repeated name keeps its last value.
  pub pairs: HashMap<String, String>,
  /// Upstream's message for the first malformed entry, if any.
  pub error: Option<String>,
}

/// Parse `NAME=VALUE` and heredoc `NAME<<DELIMITER` entries exactly like
/// upstream `EnvFileKeyValuePairs` on Linux and macOS: lines split on LF only
/// (a CR stays part of names, values and delimiters), only zero-length lines
/// are skipped, and parsing stops at the first malformed entry. An empty
/// plain name fails too: upstream has that message but checks the line
/// instead of the name, and issue #96 requires the failure.
pub fn parse_kv_file(content: &str) -> KvFile {
  let mut file = KvFile::default();
  let mut lines = Lines {
    text: content,
    index: 0,
  };
  while let Some((line, _)) = lines.next_line() {
    if line.is_empty() {
      continue;
    }
    match parse_entry(line, &mut lines) {
      Ok((key, value)) => {
        file.pairs.insert(key, value);
      },
      Err(message) => {
        file.error = Some(message);
        break;
      },
    }
  }
  file
}

/// Upstream's non-Windows `ReadLine`: lines end at LF only.
struct Lines<'a> {
  text: &'a str,
  index: usize,
}

impl<'a> Lines<'a> {
  /// The next line and whether an LF ended it; `None` at the end of the text.
  fn next_line(&mut self) -> Option<(&'a str, bool)> {
    let rest = self
      .text
      .get(self.index..)
      .filter(|rest| !rest.is_empty())?;
    if let Some(lf) = rest.find('\n') {
      self.index += lf + 1;
      Some((rest.get(..lf)?, true))
    } else {
      self.index = self.text.len();
      Some((rest, false))
    }
  }
}

/// Parse the entry starting at `line`; whichever of `=` and `<<` comes first
/// decides between the plain and the heredoc form.
fn parse_entry(line: &str, lines: &mut Lines<'_>) -> Result<(String, String), String> {
  match (line.find('='), line.find("<<")) {
    (Some(equals), Some(heredoc)) if heredoc < equals => parse_heredoc(line, lines),
    (Some(_), _) => {
      let (key, value) = line.split_once('=').unwrap_or_default();
      if key.is_empty() {
        return Err(format!("Invalid format '{line}'. Name must not be empty"));
      }
      Ok((key.to_owned(), value.to_owned()))
    },
    (None, Some(_)) => parse_heredoc(line, lines),
    (None, None) => Err(format!("Invalid format '{line}'")),
  }
}

/// Read a heredoc value up to the line that equals the delimiter exactly.
fn parse_heredoc(line: &str, lines: &mut Lines<'_>) -> Result<(String, String), String> {
  let (key, delimiter) = line.split_once("<<").unwrap_or_default();
  if key.is_empty() || delimiter.is_empty() {
    return Err(format!(
      "Invalid format '{line}'. Name must not be empty and delimiter must not be empty"
    ));
  }
  let mut value = Vec::new();
  loop {
    match lines.next_line() {
      None => {
        return Err(format!(
          "Invalid value. Matching delimiter not found '{delimiter}'"
        ));
      },
      Some((next, _)) if next == delimiter => break,
      Some((_, false)) => return Err("Invalid value. EOF marker missing new line.".to_owned()),
      Some((next, true)) => value.push(next),
    }
  }
  Ok((key.to_owned(), value.join("\n")))
}

#[cfg(test)]
#[path = "tests/file_commands.rs"]
mod tests;
