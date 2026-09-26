//! Real interpreter probes for #80; no shell/process responses are mocked.

use std::collections::HashMap;
use std::error::Error;

use execution::execution::handlers::script::{ScriptHandler, ScriptParams};
use shared::{Conclusion, RunnerError};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

async fn run(
  shell: Option<&str>,
  script: &str,
  path: &str,
) -> TestResult<(Result<Conclusion, RunnerError>, Vec<String>)> {
  let dir = tempfile::Builder::new()
    .prefix("shell 80 space ")
    .tempdir()?;
  let env = HashMap::from([("PATH".to_owned(), path.to_owned())]);
  let cancel = CancellationToken::new();
  let (events, _rx) = mpsc::channel(256);
  let (stdout, mut out) = mpsc::channel(256);
  let result = ScriptHandler::new()
    .execute(
      &ScriptParams {
        script,
        shell,
        env: &env,
        working_dir: dir.path(),
        step_id: "shell80",
        cgroup_path: None,
        timeout: Some(std::time::Duration::from_secs(15)),
        cancel: &cancel,
        container: None,
      },
      &events,
      stdout,
    )
    .await
    .map(|output| output.conclusion);
  let mut lines = Vec::new();
  while let Some(line) = out.recv().await {
    lines.push(line);
  }
  Ok((result, lines))
}

fn path() -> TestResult<String> {
  std::env::var("PATH")
    .map_err(|error| format!("PATH must be set for shell-template tests: {error}").into())
}

#[tokio::test]
async fn default_pipeline_succeeds_explicit_bash_fails() -> TestResult {
  assert_eq!(
    run(None, "false | true", &path()?).await?.0?,
    Conclusion::Success
  );
  assert_eq!(
    run(Some(""), "false | true", &path()?).await?.0?,
    Conclusion::Success
  );
  assert_eq!(
    run(Some("bash"), "false | true", &path()?).await?.0?,
    Conclusion::Failure
  );
  Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn missing_path_does_not_search_the_working_directory() -> TestResult {
  const CHILD: &str = "TOOLU_SHELL_TEMPLATES_NO_PATH_CHILD";
  if std::env::var_os(CHILD).is_none() {
    let output = std::process::Command::new(std::env::current_exe()?)
      .args([
        "--exact",
        "missing_path_does_not_search_the_working_directory",
      ])
      .env(CHILD, "1")
      .env_remove("PATH")
      .output()?;
    assert!(
      output.status.success(),
      "child stdout:\n{}\nchild stderr:\n{}",
      String::from_utf8_lossy(&output.stdout),
      String::from_utf8_lossy(&output.stderr)
    );
    return Ok(());
  }

  assert!(std::env::var_os("PATH").is_none());
  let cwd = tempfile::tempdir()?;
  std::os::unix::fs::symlink("/bin/bash", cwd.path().join("bash"))?;
  std::os::unix::fs::symlink("/bin/sh", cwd.path().join("sh"))?;
  let env = HashMap::new();
  for (shell, expected) in [(Some("bash"), "bash"), (None, "sh")] {
    let cancel = CancellationToken::new();
    let (events, _events_rx) = mpsc::channel(8);
    let (stdout, mut output) = mpsc::channel(8);
    let result = ScriptHandler::new()
      .execute(
        &ScriptParams {
          script: "printf 'SHOULD_NOT_RUN\\n'",
          shell,
          env: &env,
          working_dir: cwd.path(),
          step_id: "missing-path",
          cgroup_path: None,
          timeout: None,
          cancel: &cancel,
          container: None,
        },
        &events,
        stdout,
      )
      .await;
    let error = result
      .err()
      .ok_or("missing PATH unexpectedly ran a shell")?;
    assert!(error.to_string().contains(expected), "{error}");
    assert!(output.recv().await.is_none());
  }
  Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn default_search_falls_back_to_real_sh_and_skips_nonexecutable_bash() -> TestResult {
  use std::os::unix::fs::{PermissionsExt, symlink};
  let bin = tempfile::tempdir()?;
  symlink("/bin/sh", bin.path().join("sh"))?;
  let search = bin.path().to_str().ok_or("nonunicode temp")?;
  assert_eq!(
    run(None, "printf 'fallback\\n'", search).await?.1,
    ["fallback"]
  );
  std::fs::write(bin.path().join("bash"), "not executable")?;
  std::fs::set_permissions(
    bin.path().join("bash"),
    std::fs::Permissions::from_mode(0o600),
  )?;
  assert_eq!(
    run(None, "printf 'fallback\\n'", search).await?.1,
    ["fallback"]
  );
  std::fs::remove_file(bin.path().join("bash"))?;
  symlink("/bin/bash", bin.path().join("bash"))?;
  assert_eq!(
    run(None, "test -n \"$BASH_VERSION\"; false | true", search)
      .await?
      .0?,
    Conclusion::Success
  );
  Ok(())
}

#[tokio::test]
async fn custom_perl_and_quoted_arguments_use_requested_interpreter() -> TestResult {
  let (result, lines) = run(
    Some("perl {0} 'hello world'"),
    "print join('|', @ARGV), qq(\\n);",
    &path()?,
  )
  .await?;
  assert_eq!(result?, Conclusion::Success);
  assert_eq!(lines, ["hello world"]);
  let (result, lines) = run(
    Some("bash {0} 'one two' \"three four\""),
    "printf '%s|%s\\n' \"$1\" \"$2\"",
    &path()?,
  )
  .await?;
  assert_eq!(result?, Conclusion::Success);
  assert_eq!(lines, ["one two|three four"]);
  let (result, lines) = run(
    Some("bash {0} ~ $HOME"),
    "printf '%s|%s\\n' \"$1\" \"$2\"",
    &path()?,
  )
  .await?;
  assert_eq!(result?, Conclusion::Success);
  assert_eq!(lines, ["~|$HOME"]);
  Ok(())
}

#[tokio::test]
async fn invalid_shells_fail_before_running_script() -> TestResult {
  for shell in [
    "unknown80",
    "perl",
    "bash -e",
    "bash '{0}",
    "bash {1}",
    "bash {0} {",
    "bash {0}\\",
    " ",
    "bash {0}\0",
  ] {
    let (result, lines) = run(Some(shell), "echo SHOULD_NOT_RUN", &path()?).await?;
    assert!(result.is_err(), "{shell:?}: {result:?}");
    assert!(lines.is_empty(), "{shell:?}: {lines:?}");
  }
  let (result, lines) = run(Some("missing-shell80 {0}"), "echo SHOULD_NOT_RUN", &path()?).await?;
  assert!(
    result
      .err()
      .ok_or("expected missing executable error")?
      .to_string()
      .contains("missing-shell80")
  );
  assert!(lines.is_empty());
  Ok(())
}

#[tokio::test]
async fn repeated_placeholder_and_literal_braces_preserve_arguments() -> TestResult {
  let (result, lines) = run(
    Some("bash {0} {0} '{{literal}}'"),
    "test \"$0\" = \"$1\"; printf '%s\\n' \"$2\"",
    &path()?,
  )
  .await?;
  assert_eq!(result?, Conclusion::Success);
  assert_eq!(lines, ["{literal}"]);
  Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn custom_executable_path_with_spaces_is_preserved() -> TestResult {
  let bin = tempfile::Builder::new().prefix("shell binary ").tempdir()?;
  let exe = bin.path().join("custom bash");
  std::os::unix::fs::symlink("/bin/bash", &exe)?;
  let template = format!("'{}' {{0}}", exe.display());
  assert_eq!(
    run(Some(&template), "printf 'custom-path\\n'", &path()?)
      .await?
      .1,
    ["custom-path"]
  );
  Ok(())
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires python on PATH; run explicitly for interpreter acceptance"]
async fn python_uses_python_and_py_extension_and_native_exit() -> TestResult {
  // Only `python` exists in this PATH: rewriting it to python3 must fail.
  let interpreter = std::process::Command::new("python")
    .args(["-c", "import sys; print(sys.executable)"])
    .output()?;
  assert!(interpreter.status.success());
  let executable = String::from_utf8(interpreter.stdout)?;
  let bin = tempfile::tempdir()?;
  std::os::unix::fs::symlink(executable.trim(), bin.path().join("python"))?;
  let (result, lines) = run(
    Some("python"),
    "import sys; assert __file__.endswith('.py'); print('python80'); sys.exit(7)",
    bin.path().to_str().ok_or("nonunicode Python path")?,
  )
  .await?;
  assert_eq!(result?, Conclusion::Failure);
  assert_eq!(lines, ["python80"]);
  Ok(())
}

#[tokio::test]
#[ignore = "requires pwsh on PATH; run explicitly for interpreter acceptance"]
async fn pwsh_extension_errors_and_native_exit() -> TestResult {
  let (result, lines) = run(Some("pwsh"), "if ([IO.Path]::GetExtension($PSCommandPath) -ne '.ps1') { throw 'extension' }; Write-Output 'pwsh80'; /bin/sh -c 'exit 7'", &path()?).await?;
  assert_eq!(result?, Conclusion::Failure);
  assert_eq!(lines, ["pwsh80"]);
  let (success, output) = run(Some("pwsh"), "Write-Output 'success80'", &path()?).await?;
  assert_eq!(success?, Conclusion::Success);
  assert_eq!(output, ["success80"]);
  let (result, lines) = run(
    Some("pwsh"),
    "Write-Error 'expected80'; Write-Output 'SHOULD_NOT_RUN'",
    &path()?,
  )
  .await?;
  assert_eq!(result?, Conclusion::Failure);
  assert!(lines.is_empty());
  Ok(())
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires pwsh on PATH; run explicitly for interpreter acceptance"]
async fn uppercase_pwsh_builtin_and_custom_templates_keep_fixups() -> TestResult {
  let interpreter = std::process::Command::new("pwsh")
    .args([
      "-NoProfile",
      "-Command",
      "[Console]::Out.Write([System.Diagnostics.Process]::GetCurrentProcess().MainModule.FileName)",
    ])
    .output()?;
  assert!(interpreter.status.success());
  let executable = String::from_utf8(interpreter.stdout)?;
  let bin = tempfile::tempdir()?;
  std::os::unix::fs::symlink(executable.trim(), bin.path().join("PWSH"))?;
  let path = bin.path().to_str().ok_or("nonunicode pwsh path")?;

  let (result, lines) = run(
    Some("PWSH"),
    "if ([IO.Path]::GetExtension($PSCommandPath) -ne '.ps1') { throw 'extension' }; Write-Output 'builtin80'; /bin/sh -c 'exit 7'",
    path,
  )
  .await?;
  assert_eq!(result?, Conclusion::Failure);
  assert_eq!(lines, ["builtin80"]);

  let (result, lines) = run(
    Some("PWSH -command \". '{0}'\""),
    "Write-Error 'expected80'; Write-Output 'SHOULD_NOT_RUN'",
    path,
  )
  .await?;
  assert_eq!(result?, Conclusion::Failure);
  assert!(lines.is_empty());
  Ok(())
}

#[tokio::test]
async fn explicit_missing_interpreters_do_not_fallback() -> TestResult {
  let empty = tempfile::tempdir()?;
  for shell in ["bash", "sh", "python", "pwsh"] {
    let (result, _) = run(
      Some(shell),
      "echo nope",
      empty.path().to_str().ok_or("path")?,
    )
    .await?;
    assert!(
      result
        .err()
        .ok_or("missing interpreter succeeded")?
        .to_string()
        .contains(shell)
    );
  }
  Ok(())
}
