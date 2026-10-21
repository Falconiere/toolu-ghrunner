//! Shell-out tests for the `update` row of `toolu-runner doctor`: the latest
//! release check against loopback mirrors (failures are `SKIP`s, a newer
//! release is a `WARN` with the installer), the first registration's
//! `[network]` reaching it, and that credentials in the release URL are masked
//! everywhere they could surface.

use base64::Engine as _;

use crate::check_servers::{OK, Recorder, closed_port, run};
use crate::doctor_env::{
  HTTP_500, RELEASES_LATEST, VERSION, doctor_cmd, fix, implied_summary, json_reply, last_line,
  report, row, section, write_registration, write_registration_with,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A release document whose `tag_name` carries the password of the release
/// URL — a mirror that echoes what it was sent.
const RELEASE_ECHOES_PASSWORD: &str =
  json_reply!("200 OK", "", "{\"tag_name\":\"v9.9.9-pw-SECRET\"}");

/// A mirror URL's `user:password` masks the password, never the user name:
/// masking `toolu` would blank it in every line, the header included.
#[test]
fn releases_url_user_name_is_not_masked() -> TestResult {
  let home = tempfile::tempdir()?;
  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.arg("--offline").env(
    "TOOLU_RUNNER_RELEASES_URL",
    "http://toolu:pw-SECRET@127.0.0.1:1/latest",
  ))?;
  assert_eq!(code, Some(0), "{}", report(&stdout, &stderr));
  assert!(
    stdout
      .lines()
      .next()
      .is_some_and(|line| line.starts_with("toolu-runner doctor — ")),
    "{stdout}"
  );
  assert_eq!(
    section(&stdout, "install"),
    [
      row(
        "OK",
        "version",
        &format!(
          "toolu-runner {VERSION} (GitHub runner compatibility {})",
          protocol::runner_version::COMPATIBILITY_VERSION
        )
      ),
      row("SKIP", "update", "offline"),
    ],
    "{stdout}"
  );
  assert!(!stdout.contains("***"), "{stdout}");
  Ok(())
}

#[test]
fn releases_url_credentials_are_masked() -> TestResult {
  let home = tempfile::tempdir()?;
  let target = Recorder::spawn(|_| Some(OK))?;
  write_registration(
    home.path(),
    "o",
    "r",
    &format!("http://127.0.0.1:{}", target.port()),
    true,
  )?;

  // A mirror echoing the password into the document `doctor` prints from.
  assert!(
    RELEASE_ECHOES_PASSWORD.contains("pw-SECRET"),
    "vacuous reply"
  );
  let mirror = Recorder::spawn(|_| Some(RELEASE_ECHOES_PASSWORD))?;
  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.args(["--timeout", "5"]).env(
    "TOOLU_RUNNER_RELEASES_URL",
    format!("http://u:pw-SECRET@127.0.0.1:{}/latest", mirror.port()),
  ))?;
  assert_eq!(code, Some(0), "{}", report(&stdout, &stderr));
  let sent = mirror
    .heads()
    .into_iter()
    .flat_map(|head| {
      head
        .lines()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
    })
    .find(|line| line.starts_with("authorization:"))
    .ok_or("the release request carried no credentials")?;
  let basic = base64::engine::general_purpose::STANDARD.encode("u:pw-SECRET");
  assert_eq!(
    sent,
    format!("authorization: basic {}", basic.to_ascii_lowercase())
  );
  assert_eq!(
    section(&stdout, "install").last().copied(),
    Some(
      row(
        "SKIP",
        "update",
        &format!("cannot compare {VERSION} with the latest release v9.9.9-***")
      )
      .as_str()
    ),
    "{stdout}"
  );
  assert!(!stdout.contains("pw-SECRET"), "{stdout}");
  assert!(!stderr.contains("pw-SECRET"), "{stderr}");

  // An unreachable mirror: the transport error names the URL without its
  // userinfo, and nothing else prints it.
  let closed = closed_port()?;
  let (_, stdout, stderr, _) = run(doctor_cmd(home.path())?.args(["--timeout", "5"]).env(
    "TOOLU_RUNNER_RELEASES_URL",
    format!("http://u:pw-SECRET@127.0.0.1:{closed}/latest"),
  ))?;
  let skip = section(&stdout, "install")
    .into_iter()
    .find(|line| line.starts_with("  SKIP  update "))
    .ok_or_else(|| format!("no SKIP update row:\n{stdout}"))?;
  assert!(
    skip.starts_with(&row(
      "SKIP",
      "update",
      "could not check latest release: connect: error sending request: "
    )),
    "{skip}"
  );
  assert!(!stdout.contains("pw-SECRET"), "{stdout}");
  assert!(!stderr.contains("pw-SECRET"), "{stderr}");
  Ok(())
}

/// Run `doctor` against an ephemeral registration on a healthy loopback peer
/// (so no row but the release check can fail), with `reply` serving the
/// release document. Returns (exit code, stdout, stderr).
fn release_run(
  reply: fn(&str) -> Option<&'static str>,
) -> Result<(Option<i32>, String, String), Box<dyn std::error::Error>> {
  let home = tempfile::tempdir()?;
  let target = Recorder::spawn(|_| Some(OK))?;
  write_registration(
    home.path(),
    "o",
    "r",
    &format!("http://127.0.0.1:{}", target.port()),
    true,
  )?;
  let mirror = Recorder::spawn(reply)?;
  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.args(["--timeout", "5"]).env(
    "TOOLU_RUNNER_RELEASES_URL",
    format!("http://127.0.0.1:{}/latest", mirror.port()),
  ))?;
  Ok((code, stdout, stderr))
}

/// The `update` row of a report.
fn update_row(stdout: &str) -> Option<&str> {
  section(stdout, "install")
    .into_iter()
    .find(|line| line.contains("  update "))
}

#[test]
fn update_check_failures_are_skips() -> TestResult {
  let (code, stdout, stderr) = release_run(|_| Some(HTTP_500))?;
  assert_eq!(
    update_row(&stdout),
    Some(row("SKIP", "update", "could not check latest release: HTTP 500").as_str()),
    "{stdout}"
  );
  assert_eq!(code, Some(0), "{}", report(&stdout, &stderr));

  let (code, stdout, stderr) =
    release_run(|_| Some(json_reply!("200 OK", "", "<html>not json</html>")))?;
  assert_eq!(
    update_row(&stdout),
    Some(
      row(
        "SKIP",
        "update",
        "could not check latest release: unreadable response: error decoding response body"
      )
      .as_str()
    ),
    "{stdout}"
  );
  assert_eq!(code, Some(0), "{}", report(&stdout, &stderr));

  let (code, stdout, stderr) =
    release_run(|_| Some(json_reply!("200 OK", "", "{\"name\":\"v1.0.0\"}")))?;
  assert_eq!(
    update_row(&stdout),
    Some(
      row(
        "SKIP",
        "update",
        "could not check latest release: the response has no tag_name"
      )
      .as_str()
    ),
    "{stdout}"
  );
  assert_eq!(code, Some(0), "{}", report(&stdout, &stderr));
  Ok(())
}

#[test]
fn newer_release_is_a_warning_with_the_installer() -> TestResult {
  let (code, stdout, stderr) =
    release_run(|_| Some(json_reply!("200 OK", "", "{\"tag_name\":\"v99.0.0\"}")))?;
  assert_eq!(
    section(&stdout, "install").get(1..),
    Some(
      &[
        row(
          "WARN",
          "update",
          &format!("v99.0.0 is available (installed {VERSION})")
        )
        .as_str(),
        fix("curl -fsSL https://get.toolu.sh/pkg/ghrunner/install | bash").as_str(),
      ][..]
    ),
    "{stdout}"
  );
  // A newer release is a warning, never a failure.
  assert_eq!(code, Some(0), "{}", report(&stdout, &stderr));
  assert_eq!(last_line(&stdout), implied_summary(&stdout, 0), "{stdout}");
  Ok(())
}

#[test]
fn release_check_uses_the_first_registrations_network() -> TestResult {
  let home = tempfile::tempdir()?;
  let target = Recorder::spawn(|_| Some(OK))?;
  let base = format!("http://127.0.0.1:{}", target.port());
  // A forward proxy that answers the (otherwise unresolvable) release host.
  let proxy = Recorder::spawn(|head| {
    Some(if head.starts_with("GET http://releases.invalid/") {
      RELEASES_LATEST
    } else {
      OK
    })
  })?;
  let proxy_url = format!("http://127.0.0.1:{}", proxy.port());
  // `a/first` sorts first, so its `[network]` is the release check's.
  write_registration_with(home.path(), "a", "first", &base, true, |config| {
    config.network.proxy = Some(proxy_url.clone());
  })?;
  write_registration(home.path(), "b", "second", &base, true)?;

  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.args(["--timeout", "5"]).env(
    "TOOLU_RUNNER_RELEASES_URL",
    "http://releases.invalid/latest",
  ))?;
  assert!(
    proxy
      .request_lines()
      .contains(&"GET http://releases.invalid/latest HTTP/1.1".to_owned()),
    "{:?}",
    proxy.request_lines()
  );
  let install = section(&stdout, "install");
  assert!(
    install.contains(&row("OK", "update", "up to date (latest v0.11.1)").as_str())
      || install.contains(&row("OK", "update", "newer than the latest release (v0.11.1)").as_str()),
    "{}",
    report(&stdout, &stderr)
  );
  assert_eq!(code, Some(0), "{}", report(&stdout, &stderr));
  Ok(())
}
