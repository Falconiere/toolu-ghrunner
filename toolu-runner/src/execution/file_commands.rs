use std::collections::HashMap;
use std::path::{Path, PathBuf};

use shared::RunnerError;

/// Manages temp files for GitHub Actions file commands.
pub struct FileCommandManager {
  pub env_path: PathBuf,
  pub output_path: PathBuf,
  pub path_path: PathBuf,
  pub state_path: PathBuf,
  pub summary_path: PathBuf,
}

/// Results from processing file commands after a step.
pub struct FileCommandResults {
  pub env_vars: HashMap<String, String>,
  pub outputs: HashMap<String, String>,
  pub path_additions: Vec<String>,
  pub state: HashMap<String, String>,
  pub summary: String,
}

impl FileCommandManager {
  /// Create temp files for all file commands.
  ///
  /// Returns the manager and a map of env var names to file paths.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Io` if file creation fails.
  pub fn create(temp_dir: &Path) -> Result<(Self, HashMap<String, String>), RunnerError> {
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
      std::fs::write(path, "")?;
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

  /// Read and parse all file command files after a step.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Io` if reading files fails.
  pub fn process(&self) -> Result<FileCommandResults, RunnerError> {
    let env_content = std::fs::read_to_string(&self.env_path)?;
    let output_content = std::fs::read_to_string(&self.output_path)?;
    let path_content = std::fs::read_to_string(&self.path_path)?;
    let state_content = std::fs::read_to_string(&self.state_path)?;
    let summary = std::fs::read_to_string(&self.summary_path)?;

    let mut env_vars = parse_env_file(&env_content);
    // NODE_OPTIONS is blocked (case-insensitive)
    env_vars.retain(|k, _| !k.eq_ignore_ascii_case("NODE_OPTIONS"));

    Ok(FileCommandResults {
      env_vars,
      outputs: parse_output_file(&output_content),
      path_additions: parse_path_file(&path_content),
      state: parse_kv_file(&state_content),
      summary: truncate_summary(summary),
    })
  }

  /// Reset all file command files for the next step.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Io` if writing fails.
  pub fn reset(&self) -> Result<(), RunnerError> {
    for path in [
      &self.env_path,
      &self.output_path,
      &self.path_path,
      &self.state_path,
      &self.summary_path,
    ] {
      std::fs::write(path, "")?;
    }
    Ok(())
  }
}

/// Parse a GITHUB_ENV file. Supports `KEY=VALUE` and heredoc `KEY<<DELIM`.
///
/// Splits on first `=` only (values may contain `=`).
/// Handles both `\r\n` and `\n` line endings.
/// Skips blank lines.
pub fn parse_env_file(content: &str) -> HashMap<String, String> {
  parse_kv_file(content)
}

/// Parse a GITHUB_OUTPUT file (same format as env).
pub fn parse_output_file(content: &str) -> HashMap<String, String> {
  parse_kv_file(content)
}

/// Parse a GITHUB_PATH file — one path per line.
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
