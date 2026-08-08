use std::collections::HashMap;
use std::path::{Path, PathBuf};

use shared::RunnerError;

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
  /// Contents written to `GITHUB_STEP_SUMMARY`, truncated to the 1 MiB limit.
  pub summary: String,
}

impl FileCommandManager {
  /// Create temp files for all file commands.
  ///
  /// Returns the manager and a map of env var names to file paths. Async
  /// (`tokio::fs`, this crate's idiom for filesystem work reached from an
  /// async context — see `artifacts/backend.rs`, `cache/blob/put_blob.rs`)
  /// because every caller runs on the async step-execution path; a blocking
  /// `std::fs` call here would stall the executor thread for the duration of
  /// five file creations.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Io` if file creation fails.
  pub async fn create(temp_dir: &Path) -> Result<(Self, HashMap<String, String>), RunnerError> {
    let mgr = Self {
      env_path: temp_dir.join("github_env"),
      output_path: temp_dir.join("github_output"),
      path_path: temp_dir.join("github_path"),
      state_path: temp_dir.join("github_state"),
      summary_path: temp_dir.join("github_step_summary"),
    };

    // Create empty files
    for path in [
      &mgr.env_path,
      &mgr.output_path,
      &mgr.path_path,
      &mgr.state_path,
      &mgr.summary_path,
    ] {
      tokio::fs::write(path, "").await?;
    }

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

  /// Read and parse all file command files after a step. Async (`tokio::fs`)
  /// for the same reason as [`Self::create`].
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Io` if reading files fails.
  pub async fn process(&self) -> Result<FileCommandResults, RunnerError> {
    let env_content = tokio::fs::read_to_string(&self.env_path).await?;
    let output_content = tokio::fs::read_to_string(&self.output_path).await?;
    let path_content = tokio::fs::read_to_string(&self.path_path).await?;
    let state_content = tokio::fs::read_to_string(&self.state_path).await?;
    let summary = tokio::fs::read_to_string(&self.summary_path).await?;

    let mut env_vars = parse_env_file(&env_content);
    strip_blocked_env(&mut env_vars);

    Ok(FileCommandResults {
      env_vars,
      outputs: parse_output_file(&output_content),
      path_additions: parse_path_file(&path_content),
      state: parse_kv_file(&state_content),
      summary: truncate_summary(summary),
    })
  }

  /// Reset all file command files for the next step. Async (`tokio::fs`) for
  /// the same reason as [`Self::create`].
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Io` if writing fails.
  pub async fn reset(&self) -> Result<(), RunnerError> {
    for path in [
      &self.env_path,
      &self.output_path,
      &self.path_path,
      &self.state_path,
      &self.summary_path,
    ] {
      tokio::fs::write(path, "").await?;
    }
    Ok(())
  }
}

/// Strip env keys the runner refuses to propagate from a `$GITHUB_ENV`
/// read-back. Today that is `NODE_OPTIONS` (case-insensitive): node children
/// honor it (e.g. `--require`), so letting a `run:` step set it via
/// `$GITHUB_ENV` would inject a preload into every later node action — the one
/// env guard the runner enforces. Applied to BOTH the top-level file-command
/// read-back and the composite `$GITHUB_ENV` read-back.
pub fn strip_blocked_env<S: ::std::hash::BuildHasher>(env: &mut HashMap<String, String, S>) {
  env.retain(|k, _| !k.eq_ignore_ascii_case("NODE_OPTIONS"));
}

/// Parse a `GITHUB_ENV` file. Supports `KEY=VALUE` and heredoc `KEY<<DELIM`.
///
/// Splits on first `=` only (values may contain `=`).
/// Handles both `\r\n` and `\n` line endings.
/// Skips blank lines.
pub fn parse_env_file(content: &str) -> HashMap<String, String> {
  parse_kv_file(content)
}

/// Parse a `GITHUB_OUTPUT` file (same format as env).
pub fn parse_output_file(content: &str) -> HashMap<String, String> {
  parse_kv_file(content)
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

fn parse_kv_file(content: &str) -> HashMap<String, String> {
  let mut result = HashMap::new();
  let lines: Vec<&str> = content.split('\n').collect();
  let mut i = 0;

  while i < lines.len() {
    let line = lines
      .get(i)
      .copied()
      .unwrap_or_default()
      .trim_end_matches('\r');
    i += 1;

    if line.is_empty() {
      continue;
    }

    // Check for heredoc: KEY<<DELIMITER
    if let Some((key, delimiter)) = parse_heredoc_start(line) {
      let mut value_lines: Vec<&str> = Vec::new();
      while i < lines.len() {
        let next = lines
          .get(i)
          .copied()
          .unwrap_or_default()
          .trim_end_matches('\r');
        i += 1;
        if next == delimiter {
          break;
        }
        value_lines.push(next);
      }
      result.insert(key.to_owned(), value_lines.join("\n"));
      continue;
    }

    // Simple KEY=VALUE (split on first = only)
    if let Some((key, value)) = line.split_once('=')
      && !key.is_empty()
    {
      result.insert(key.to_owned(), value.to_owned());
    }
  }

  result
}

fn parse_heredoc_start(line: &str) -> Option<(&str, &str)> {
  let pos = line.find("<<")?;
  let key = line.get(..pos)?;
  let delimiter = line.get(pos + 2..)?;
  if key.is_empty() || delimiter.is_empty() {
    return None;
  }
  Some((key, delimiter))
}

/// Truncate summary to 1 MiB limit.
fn truncate_summary(summary: String) -> String {
  const MAX_SUMMARY_BYTES: usize = 1024 * 1024;
  if summary.len() <= MAX_SUMMARY_BYTES {
    return summary;
  }
  let mut end = MAX_SUMMARY_BYTES;
  while end > 0 && !summary.is_char_boundary(end) {
    end -= 1;
  }
  summary.get(..end).unwrap_or_default().to_owned()
}
