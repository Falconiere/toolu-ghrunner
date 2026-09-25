//! Tests for the job-container Docker create-option parser.

use super::parse_container_options;

/// Resource, identity, hostname, and security values retain their argv
/// boundaries after POSIX quotes are removed; Docker receives no shell string.
#[test]
fn parses_documented_options_with_quotes_as_distinct_argv_entries() {
  let result = parse_container_options(
    "--cpus=1.5 --memory 512m --user '1001:1001' --hostname build --security-opt 'no-new-privileges:true' --read-only",
  );
  assert!(result.is_ok(), "documented options must parse: {result:?}");
  if let Ok(arguments) = result {
    assert_eq!(
      arguments,
      vec![
        "--cpus".to_owned(),
        "1.5".to_owned(),
        "--memory".to_owned(),
        "512m".to_owned(),
        "--user".to_owned(),
        "1001:1001".to_owned(),
        "--hostname".to_owned(),
        "build".to_owned(),
        "--security-opt".to_owned(),
        "no-new-privileges:true".to_owned(),
        "--read-only".to_owned(),
      ]
    );
  }
}

/// Docker health flags keep the command as one argv value, so the typed
/// translator can send Docker's `CMD-SHELL` contract without invoking a shell.
#[test]
fn parses_health_options_with_quoted_command() {
  let result = parse_container_options(
    "--health-cmd 'curl --fail http://localhost:8080/ready' --health-interval=2s --health-retries 3 --health-start-period 1m --health-start-interval 500ms --health-timeout 1s",
  );

  assert!(result.is_ok(), "health options must parse: {result:?}");
  if let Ok(arguments) = result {
    assert_eq!(
      arguments,
      vec![
        "--health-cmd".to_owned(),
        "curl --fail http://localhost:8080/ready".to_owned(),
        "--health-interval".to_owned(),
        "2s".to_owned(),
        "--health-retries".to_owned(),
        "3".to_owned(),
        "--health-start-period".to_owned(),
        "1m".to_owned(),
        "--health-start-interval".to_owned(),
        "500ms".to_owned(),
        "--health-timeout".to_owned(),
        "1s".to_owned(),
      ]
    );
  }
}

/// The runner owns container topology, identity, entrypoint, workdir, and
/// mounts, so workflow options cannot replace them.
#[test]
fn rejects_runner_owned_options() {
  for option in [
    "--name another",
    "--network host",
    "--entrypoint /bin/bash",
    "--workdir /tmp",
    "--mount type=bind,src=/tmp,dst=/tmp",
    "-v /tmp:/tmp",
  ] {
    let result = parse_container_options(option);
    let message = match result {
      Err(error) => error.to_string(),
      Ok(arguments) => format!("accepted arguments: {arguments:?}"),
    };
    assert!(
      message.contains("reserved by the runner"),
      "{option}: {message}"
    );
  }
}

/// Docker create arguments after the option list would be an injected command
/// or image, neither of which is part of a job-container declaration.
#[test]
fn rejects_unknown_flags_and_positionals() {
  for option in ["--label team=runner", "ubuntu:24.04", "-- --rm", "--cpus"] {
    let result = parse_container_options(option);
    assert!(result.is_err(), "option must be rejected: {option}");
  }
}

/// Unbalanced shell syntax is invalid input, never something the runner hands
/// to a command interpreter for a second chance to parse.
#[test]
fn rejects_invalid_quoting() {
  let result = parse_container_options("--hostname 'unfinished");
  let message = match result {
    Err(error) => error.to_string(),
    Ok(arguments) => format!("accepted arguments: {arguments:?}"),
  };
  assert!(message.contains("invalid quoting"), "{message}");
}
