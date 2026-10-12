//! Issue #96: upstream `EnvFileKeyValuePairs` grammar over bodies written by
//! real Bash `printf`, read back through the production command-file reader.
use std::collections::HashMap;
use std::error::Error;
use std::path::Path;

use super::{KvFile, parse_kv_file, read_command_file};

type TestResult = Result<(), Box<dyn Error>>;

/// Have real Bash write `format` (a `printf` format string) to a file and read
/// it back through the reader every file command uses.
fn bash_written(dir: &Path, format: &str) -> Result<String, Box<dyn Error>> {
  let path = dir.join("github_env");
  let status = std::process::Command::new("bash")
    .args(["-c", "printf \"$1\" >> \"$2\"", "bash", format])
    .arg(&path)
    .status()?;
  assert!(status.success(), "bash printf failed for {format:?}");
  Ok(read_command_file(&path)?)
}

fn parsed(format: &str) -> Result<KvFile, Box<dyn Error>> {
  let dir = tempfile::tempdir()?;
  Ok(parse_kv_file(&bash_written(dir.path(), format)?))
}

fn pairs(entries: &[(&str, &str)]) -> HashMap<String, String> {
  entries
    .iter()
    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
    .collect()
}

#[test]
fn valid_plain_lines_match_upstream() -> TestResult {
  let file =
    parsed("EMPTY=\\nEQ=b=c\\nMIXED=x<<y\\nU=h\\xc3\\xa9llo \\xe2\\x9c\\x93\\nDUP=1\\nDUP=2\\n")?;
  assert_eq!(file.error, None);
  assert_eq!(
    file.pairs,
    pairs(&[
      ("EMPTY", ""),
      ("EQ", "b=c"),
      ("MIXED", "x<<y"),
      ("U", "héllo ✓"),
      ("DUP", "2"),
    ])
  );
  Ok(())
}

#[test]
fn crlf_keeps_carriage_returns_like_the_linux_runner() -> TestResult {
  let file = parsed("A=1\\r\\nN<<EOF\\r\\nv\\r\\nEOF\\r\\n")?;
  assert_eq!(file.error, None);
  assert_eq!(file.pairs, pairs(&[("A", "1\r"), ("N", "v\r")]));
  Ok(())
}

#[test]
fn heredoc_values_and_delimiter_boundaries() -> TestResult {
  let file = parsed(
    "EMPTY<<EOF\\nEOF\\nMULTI<<EOF\\na\\n\\nb\\nEOF\\nLIKE<<EOF\\nEOF2\\n EOF\\nEOF\\nTAIL<<END\\nv\\nEND",
  )?;
  assert_eq!(file.error, None);
  assert_eq!(
    file.pairs,
    pairs(&[
      ("EMPTY", ""),
      ("MULTI", "a\n\nb"),
      ("LIKE", "EOF2\n EOF"),
      ("TAIL", "v"),
    ])
  );
  Ok(())
}

#[test]
fn malformed_lines_fail_with_upstream_messages() -> TestResult {
  let cases = [
    ("just-text\\n", "Invalid format 'just-text'"),
    ("\\r\\n", "Invalid format '\r'"),
    (" \\n", "Invalid format ' '"),
    ("=v\\n", "Invalid format '=v'. Name must not be empty"),
    (
      "N<<\\n",
      "Invalid format 'N<<'. Name must not be empty and delimiter must not be empty",
    ),
    (
      "<<EOF\\n",
      "Invalid format '<<EOF'. Name must not be empty and delimiter must not be empty",
    ),
    (
      "N<<EOF\\nv\\n",
      "Invalid value. Matching delimiter not found 'EOF'",
    ),
    (
      "N<<EOF\\n",
      "Invalid value. Matching delimiter not found 'EOF'",
    ),
    ("N<<EOF\\nv", "Invalid value. EOF marker missing new line."),
  ];
  for (format, message) in cases {
    let file = parsed(format)?;
    assert_eq!(file.error.as_deref(), Some(message), "body {format:?}");
    assert!(file.pairs.is_empty(), "body {format:?}");
  }
  Ok(())
}

#[test]
fn pairs_before_the_first_error_are_kept_and_parsing_stops() -> TestResult {
  let file = parsed("OK=1\\nM<<EOF\\nm\\nEOF\\nbad\\nLATER=2\\n")?;
  assert_eq!(file.error.as_deref(), Some("Invalid format 'bad'"));
  assert_eq!(file.pairs, pairs(&[("OK", "1"), ("M", "m")]));
  Ok(())
}

#[test]
fn reader_strips_bom_decodes_lossily_and_treats_missing_as_empty() -> TestResult {
  let dir = tempfile::tempdir()?;
  let body = bash_written(dir.path(), "\\xef\\xbb\\xbfA=1\\nB=\\xff\\n")?;
  assert_eq!(
    parse_kv_file(&body).pairs,
    pairs(&[("A", "1"), ("B", "\u{fffd}")])
  );
  assert_eq!(read_command_file(&dir.path().join("deleted"))?, "");
  let unreadable = read_command_file(dir.path());
  assert!(
    unreadable.is_err(),
    "a directory is an I/O failure: {unreadable:?}"
  );
  Ok(())
}
