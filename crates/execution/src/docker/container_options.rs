//! Safe parsing of the restricted Docker create-option surface for job containers.

use shared::RunnerError;

/// Parse a workflow `container.options` string into Docker CLI arguments.
///
/// The parser understands POSIX quoting but does not invoke a shell. Only the
/// documented resource, identity, hostname, and security options accepted by
/// job containers are retained; topology and runner-owned settings remain
/// exclusively under the runner's control.
///
/// # Errors
///
/// Returns [`RunnerError::Config`] for malformed quoting, unsupported options,
/// missing option values, or positional arguments.
pub fn parse_container_options(input: &str) -> Result<Vec<String>, RunnerError> {
  let words = shlex::split(input).ok_or_else(|| config_error("options contain invalid quoting"))?;
  let mut parsed = Vec::with_capacity(words.len());
  let mut words = words.into_iter();

  while let Some(word) = words.next() {
    parse_option(&word, &mut words, &mut parsed)?;
  }
  Ok(parsed)
}

fn parse_option(
  word: &str,
  words: &mut std::vec::IntoIter<String>,
  parsed: &mut Vec<String>,
) -> Result<(), RunnerError> {
  if word == "--" || !word.starts_with('-') || word == "-" {
    return Err(config_error(
      "options must not contain positional arguments",
    ));
  }
  let (flag, inline_value) = split_option(word);
  if reserved_flag(flag) {
    return Err(config_error(&format!(
      "option `{flag}` is reserved by the runner"
    )));
  }
  if value_flag(flag) {
    let value = option_value(flag, inline_value, words)?;
    parsed.extend([flag.to_owned(), value]);
  } else if switch_flag(flag) && inline_value.is_none() {
    parsed.push(flag.to_owned());
  } else if switch_flag(flag) {
    return Err(config_error(&format!(
      "option `{flag}` does not take a value"
    )));
  } else {
    return Err(config_error(&format!(
      "unsupported container option `{flag}`"
    )));
  }
  Ok(())
}

fn option_value(
  flag: &str,
  inline_value: Option<&str>,
  words: &mut std::vec::IntoIter<String>,
) -> Result<String, RunnerError> {
  match inline_value {
    Some(value) if !value.is_empty() => Ok(value.to_owned()),
    Some(_) => Err(config_error(&format!("option `{flag}` requires a value"))),
    None => match words.next() {
      Some(value) if !value.starts_with("--") => Ok(value),
      Some(_) | None => Err(config_error(&format!("option `{flag}` requires a value"))),
    },
  }
}

fn split_option(word: &str) -> (&str, Option<&str>) {
  match word.split_once('=') {
    Some((flag, value)) => (flag, Some(value)),
    None => (word, None),
  }
}

fn value_flag(flag: &str) -> bool {
  matches!(
    flag,
    "--cpus"
      | "--cpu-shares"
      | "--cpu-period"
      | "--cpu-quota"
      | "--cpuset-cpus"
      | "--cpuset-mems"
      | "--memory"
      | "--memory-reservation"
      | "--memory-swap"
      | "--memory-swappiness"
      | "--pids-limit"
      | "--ulimit"
      | "--user"
      | "--group-add"
      | "--hostname"
      | "--domainname"
      | "--cap-add"
      | "--cap-drop"
      | "--security-opt"
      | "--health-cmd"
      | "--health-interval"
      | "--health-timeout"
      | "--health-retries"
      | "--health-start-period"
      | "--health-start-interval"
  )
}

fn switch_flag(flag: &str) -> bool {
  matches!(flag, "--privileged" | "--read-only" | "--no-healthcheck")
}

fn reserved_flag(flag: &str) -> bool {
  matches!(
    flag,
    "--name"
      | "--network"
      | "--net"
      | "--entrypoint"
      | "--workdir"
      | "--volume"
      | "--mount"
      | "--tmpfs"
      | "--env"
      | "--env-file"
      | "-v"
      | "-w"
      | "-e"
  )
}

fn config_error(message: &str) -> RunnerError {
  RunnerError::Config(format!("invalid job container options: {message}"))
}

#[cfg(test)]
#[path = "tests/container_options.rs"]
mod tests;
