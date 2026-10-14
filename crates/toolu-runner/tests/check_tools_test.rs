//! Shell-out tests for the `check` tool lines (#93 AC-7): the binary's
//! git / docker / node report compared against the same real binaries run
//! by the test, plus missing, hung and misconfigured tools. A healthy
//! loopback target keeps the exit code about the tools alone.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::check_servers;

use check_servers::{OK, Recorder, check_cmd, closed_port, run};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn healthy() -> std::io::Result<Recorder> {
  Recorder::spawn(|_| Some(OK))
}

/// What the real tool prints, or `None` when it is not installed here.
fn real_output(program: &str, args: &[&str]) -> Option<String> {
  let output = Command::new(program).args(args).output().ok()?;
  output
    .status
    .success()
    .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[test]
fn tool_versions_match_the_real_binaries() -> TestResult {
  let home = tempfile::tempdir()?;
  let target = healthy()?;
  let base = format!("http://127.0.0.1:{}", target.port());
  let (code, stdout, _, _) =
    run(check_cmd(home.path())?.args(["--url", &base, "--timeout", "20"]))?;
  assert_eq!(code, Some(0), "{stdout}");
  assert_eq!(
    target.request_lines().len(),
    3,
    "{:?}",
    target.request_lines()
  );

  let mut verified = Vec::new();
  for tool in ["git", "node"] {
    match real_output(tool, &["--version"]) {
      Some(version) => {
        let line = version.lines().next().unwrap_or_default().to_owned();
        assert!(
          stdout.contains(&format!("  OK    {tool:<10} {line}")),
          "{tool}: {stdout}"
        );
        verified.push(tool);
      },
      None => assert!(
        stdout.contains(&format!("  WARN  {tool:<10} not found on PATH")),
        "{stdout}"
      ),
    }
  }
  if let Some(client) = real_output("docker", &["version", "--format", "{{.Client.Version}}"]) {
    assert!(stdout.contains(&format!("CLI {client}")), "{stdout}");
    verified.push("docker");
  }
  // Recorded so a host lacking a tool shows that lane as unverified.
  println!("verified against real binaries: {verified:?}");
  Ok(())
}

#[test]
fn missing_tools_are_optional() -> TestResult {
  let home = tempfile::tempdir()?;
  let empty_path = tempfile::tempdir()?;
  let target = healthy()?;
  let base = format!("http://127.0.0.1:{}", target.port());
  let (code, stdout, stderr, _) = run(
    check_cmd(home.path())?
      .env("PATH", empty_path.path())
      .args(["--url", &base, "--timeout", "5"]),
  )?;
  assert_eq!(code, Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
  for tool in ["git", "docker", "node"] {
    assert!(
      stdout.contains(&format!("  WARN  {tool:<10} not found on PATH (")),
      "{tool}: {stdout}"
    );
  }
  assert!(
    stdout.contains("result: all required checks passed (exit 0)"),
    "{stdout}"
  );
  Ok(())
}

#[cfg(unix)]
fn write_hanging_git(dir: &Path) -> std::io::Result<()> {
  use std::os::unix::fs::PermissionsExt;
  let git = dir.join("git");
  std::fs::write(&git, "#!/bin/sh\nexec sleep 30\n")?;
  std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755))
}

#[cfg(unix)]
#[test]
fn hung_tool_is_bounded() -> TestResult {
  let home = tempfile::tempdir()?;
  let shim = tempfile::tempdir()?;
  write_hanging_git(shim.path())?;
  let target = healthy()?;
  let base = format!("http://127.0.0.1:{}", target.port());
  let (code, stdout, _, elapsed) = run(
    check_cmd(home.path())?
      .env("PATH", format!("{}:/usr/bin:/bin", shim.path().display()))
      .args(["--url", &base, "--timeout", "2"]),
  )?;
  assert_eq!(code, Some(0), "{stdout}");
  assert!(
    stdout.contains("  WARN  git        timed out after 2 s and was killed"),
    "{stdout}"
  );
  assert!(elapsed < Duration::from_secs(10), "took {elapsed:?}");
  Ok(())
}

#[test]
fn unreachable_docker_daemon_warns_with_the_endpoint() -> TestResult {
  if real_output("docker", &["--version"]).is_none() {
    println!("UNVERIFIED: no docker CLI on this host");
    return Ok(());
  }
  let home = tempfile::tempdir()?;
  let socket = home.path().join("absent.sock");
  let target = healthy()?;
  let base = format!("http://127.0.0.1:{}", target.port());
  let (code, stdout, _, _) = run(
    check_cmd(home.path())?
      .env("DOCKER_HOST", format!("unix://{}", socket.display()))
      .args(["--url", &base, "--timeout", "10"]),
  )?;
  assert_eq!(code, Some(0), "{stdout}");
  assert!(stdout.contains("daemon unreachable:"), "{stdout}");
  assert!(
    stdout.contains(&format!("endpoint: unix://{}", socket.display())),
    "{stdout}"
  );
  Ok(())
}

#[test]
fn docker_host_credentials_are_redacted() -> TestResult {
  let home = tempfile::tempdir()?;
  let port = closed_port()?;
  let target = healthy()?;
  let base = format!("http://127.0.0.1:{}", target.port());
  let (code, stdout, stderr, _) = run(
    check_cmd(home.path())?
      .env(
        "DOCKER_HOST",
        format!("tcp://dockeruser:D0ckerS3cret93@127.0.0.1:{port}"),
      )
      .args(["--url", &base, "--timeout", "10"]),
  )?;
  assert_eq!(code, Some(0), "{stdout}");
  assert!(
    stdout.contains(&format!("endpoint: tcp://***@127.0.0.1:{port}")),
    "{stdout}"
  );
  assert!(
    !stdout.contains("D0ckerS3cret93") && !stderr.contains("D0ckerS3cret93"),
    "{stdout}"
  );
  Ok(())
}

#[cfg(unix)]
#[test]
fn docker_output_without_the_version_separator_warns() -> TestResult {
  use std::os::unix::fs::PermissionsExt;
  let home = tempfile::tempdir()?;
  let shim = tempfile::tempdir()?;
  let docker = shim.path().join("docker");
  // An old or wrapped CLI that ignores `--format` and prints prose.
  std::fs::write(
    &docker,
    "#!/bin/sh\necho 'Docker version 1.13.1, build 092cba3'\n",
  )?;
  std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755))?;
  let target = healthy()?;
  let base = format!("http://127.0.0.1:{}", target.port());
  let (code, stdout, _, _) = run(
    check_cmd(home.path())?
      .env("PATH", format!("{}:/usr/bin:/bin", shim.path().display()))
      .args(["--url", &base, "--timeout", "5"]),
  )?;
  assert_eq!(code, Some(0), "{stdout}");
  assert!(
    stdout.contains(
      "  WARN  docker     unexpected `docker version` output: Docker version 1.13.1, build 092cba3"
    ),
    "{stdout}"
  );
  Ok(())
}
