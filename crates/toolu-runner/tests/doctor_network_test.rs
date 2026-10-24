//! Shell-out tests for the `network` section and the summary line of
//! `toolu-runner doctor`: the healthy path probes the registration's own JIT
//! endpoints, `--offline` makes no requests, a closed port fails the required
//! probes with a hint, a registration whose JIT config is unreadable still
//! probes its hosts, an unusable `[network]` section fails the section, and the
//! `result:` line counts failures and warnings (singular and plural).

use std::os::unix::fs::PermissionsExt as _;

use crate::check_servers::{OK, Recorder, closed_port, run};
use crate::doctor_env::{
  REREGISTER, TEST_BEARER, block, doctor_cmd, fix, has_probe, healthy_reply, implied_summary,
  jit_ok, last_line, network_rows, report, row, section, service_install_fix, skip_endpoint,
  write_lock_body, write_registration, write_registration_with,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn unreadable_jit_still_probes_the_hosts() -> TestResult {
  let home = tempfile::tempdir()?;
  let peer = Recorder::spawn(|_| Some(OK))?;
  let base = format!("http://127.0.0.1:{}", peer.port());
  let config = write_registration_with(home.path(), "o", "r", &base, true, |config| {
    config.runtime.jit_config = "this-is-not-a-jit-config".to_owned();
  })?;

  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.args(["--timeout", "5"]))?;
  assert_eq!(code, Some(1), "{}", report(&stdout, &stderr));
  assert_eq!(
    block(&stdout, "o/r"),
    [
      row("OK", "config", &config.display().to_string()),
      row("OK", "creds", "present (0600)"),
      row(
        "FAIL",
        "jit",
        "unreadable: the registration's jit_config is not a valid JIT config envelope"
      ),
      fix(REREGISTER),
      row("WARN", "run", "not running"),
      fix(&format!("toolu-runner run --config {}", config.display())),
      row("INFO", "last job", "no jobs yet"),
    ],
    "{stdout}"
  );

  // The host is still probed, through its generic (not JIT) service URLs.
  let network = network_rows(&stdout, "127.0.0.1");
  for (name, path) in [
    ("api", "/api/v3"),
    ("pipelines", "/_services/pipelines/_apis/health"),
    ("token", "/_services/vstoken/_apis/health"),
  ] {
    assert!(
      has_probe(&network, name, &format!("{base}{path}"), 200),
      "no PASS {name} row:\n{stdout}"
    );
  }
  assert!(
    network
      .contains(&skip_endpoint("broker", "GHES uses the V1 protocol: no broker service").as_str()),
    "{stdout}"
  );
  let requested = peer.request_lines();
  assert!(
    requested.contains(&"GET /_services/pipelines/_apis/health HTTP/1.1".to_owned()),
    "{requested:?}"
  );
  assert!(
    !requested
      .iter()
      .any(|line| line.contains("/pipelines/tenant/") || line.contains("/token/oauth")),
    "the unreadable JIT's URLs were probed: {requested:?}"
  );
  Ok(())
}

/// Two registrations on one host, the first with a broken JIT: the host is
/// probed through the sibling's own JIT endpoints, not the generic fallback.
#[test]
fn a_host_prefers_a_sibling_with_a_readable_jit() -> TestResult {
  let home = tempfile::tempdir()?;
  let peer = Recorder::spawn(|_| Some(OK))?;
  let base = format!("http://127.0.0.1:{}", peer.port());
  write_registration_with(home.path(), "o", "a", &base, true, |config| {
    config.runtime.jit_config = "this-is-not-a-jit-config".to_owned();
  })?;
  write_registration(home.path(), "o", "b", &base, true)?;

  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.args(["--timeout", "5"]))?;
  assert_eq!(code, Some(1), "{}", report(&stdout, &stderr));
  assert_eq!(
    stdout
      .lines()
      .filter(|line| line.starts_with("network ("))
      .count(),
    1,
    "one network section per host:\n{stdout}"
  );
  let requested = peer.request_lines();
  assert!(
    requested
      .iter()
      .any(|line| line.contains("/pipelines/tenant/")),
    "the readable sibling's JIT endpoints were not probed: {requested:?}"
  );
  assert!(
    !requested.iter().any(|line| line.contains("/_services/")),
    "the generic fallback was probed despite a readable sibling: {requested:?}"
  );
  Ok(())
}

#[test]
fn unusable_network_settings_fail_the_network_section() -> TestResult {
  let home = tempfile::tempdir()?;
  let peer = Recorder::spawn(|_| Some(OK))?;
  let ca = home.path().join("no-such-ca.pem");
  write_registration_with(
    home.path(),
    "o",
    "r",
    &format!("http://127.0.0.1:{}", peer.port()),
    true,
    |config| {
      config.network.ca_bundle = Some(ca.display().to_string());
      // A credentialed proxy beside the broken bundle: never echoed.
      config.network.proxy = Some("http://proxyuser:pw-SECRET@127.0.0.1:9".to_owned());
    },
  )?;

  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.args(["--timeout", "5"]))?;
  assert_eq!(code, Some(1), "{}", report(&stdout, &stderr));
  assert!(!stdout.contains("pw-SECRET"), "{stdout}");
  assert!(!stderr.contains("pw-SECRET"), "{stderr}");
  let cause = format!(
    "config error: [network] ca_bundle {}: cannot access: No such file or directory (os error 2)",
    ca.display()
  );
  assert_eq!(
    network_rows(&stdout, "127.0.0.1"),
    [row(
      "FAIL",
      "network",
      &format!("unusable [network] settings: {cause}")
    )],
    "{stdout}"
  );
  // The release check goes through the same registration's `[network]`.
  assert!(
    section(&stdout, "install").contains(
      &row(
        "SKIP",
        "update",
        &format!("could not check latest release: {cause}")
      )
      .as_str()
    ),
    "{stdout}"
  );
  assert!(peer.heads().is_empty(), "probed anyway: {:?}", peer.heads());
  assert_eq!(last_line(&stdout), implied_summary(&stdout, 1), "{stdout}");
  Ok(())
}

#[test]
fn summary_counts_failures_and_warnings() -> TestResult {
  let home = tempfile::tempdir()?;
  let config = write_registration_with(
    home.path(),
    "o",
    "r",
    "http://127.0.0.1:9",
    false,
    |config| {
      config.runtime.jit_config = "this-is-not-a-jit-config".to_owned();
    },
  )?;
  let reg_dir = config.parent().ok_or("no parent")?;
  write_lock_body(reg_dir, std::process::id(), &config)?;
  std::fs::write(reg_dir.join(".pending_remove"), "")?;
  std::fs::set_permissions(
    reg_dir.join("credentials.json"),
    std::fs::Permissions::from_mode(0o644),
  )?;

  let (code, stdout, stderr, _) = run(
    doctor_cmd(home.path())?
      .arg("--offline")
      .env("TOOLU_RUNNER_TOKEN", TEST_BEARER),
  )?;
  assert_eq!(code, Some(1), "{}", report(&stdout, &stderr));
  // One failure (the JIT) and three warnings (creds, service, pending); the
  // host section adds one more on a machine that runs as root.
  let host_warnings = section(&stdout, "host")
    .iter()
    .filter(|line| line.starts_with("  WARN  "))
    .count();
  let host_failures = section(&stdout, "host")
    .iter()
    .filter(|line| line.starts_with("  FAIL  "))
    .count();
  assert_eq!(host_failures, 0, "{stdout}");
  assert_eq!(
    last_line(&stdout),
    format!("result: 1 failure, {} warnings (exit 1)", 3 + host_warnings),
    "{stdout}"
  );
  Ok(())
}

#[test]
fn summary_singular_warning() -> TestResult {
  let home = tempfile::tempdir()?;
  // Ephemeral and not running: exactly one warning (`run`).
  write_registration(home.path(), "o", "r", "http://127.0.0.1:9", true)?;
  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.arg("--offline"))?;
  assert_eq!(code, Some(0), "{}", report(&stdout, &stderr));
  let host_warnings = section(&stdout, "host")
    .iter()
    .filter(|line| line.starts_with("  WARN  "))
    .count();
  let expected = if host_warnings == 0 {
    "result: 0 failures, 1 warning (exit 0)".to_owned()
  } else {
    format!(
      "result: 0 failures, {} warnings (exit 0)",
      1 + host_warnings
    )
  };
  assert_eq!(last_line(&stdout), expected, "{stdout}");
  Ok(())
}

#[test]
fn healthy() -> TestResult {
  let home = tempfile::tempdir()?;
  let api = Recorder::spawn(|head| Some(healthy_reply(head)))?;
  let base = format!("http://127.0.0.1:{}", api.port());
  let config = write_registration(home.path(), "o", "r", &base, false)?;
  let since = write_lock_body(
    config.parent().ok_or("no parent")?,
    std::process::id(),
    &config,
  )?;

  let (code, stdout, stderr, _) = run(
    doctor_cmd(home.path())?
      .args(["--timeout", "5"])
      .env("TOOLU_RUNNER_TOKEN", TEST_BEARER),
  )?;
  assert_eq!(code, Some(0), "{}", report(&stdout, &stderr));
  assert_eq!(
    section(&stdout, "github auth (127.0.0.1)"),
    [row(
      "OK",
      "token",
      "TOOLU_RUNNER_TOKEN: valid (login Falconiere)"
    )],
    "{stdout}"
  );
  assert_eq!(
    block(&stdout, "o/r"),
    [
      row("OK", "config", &config.display().to_string()),
      row("OK", "creds", "present (0600)"),
      jit_ok(&config)?,
      row(
        "OK",
        "run",
        &format!("active (pid {}, since {since})", std::process::id())
      ),
      row("WARN", "service", "not installed"),
      service_install_fix(),
      row("INFO", "last job", "no jobs yet"),
    ],
    "{stdout}"
  );
  // The registration's own JIT endpoints are the ones probed.
  let network = network_rows(&stdout, "127.0.0.1");
  for (name, path) in [
    ("api", "/api/v3"),
    ("pipelines", "/pipelines/tenant/"),
    ("broker", "/broker/tenant/"),
    ("token", "/token/oauth"),
  ] {
    assert!(
      has_probe(&network, name, &format!("{base}{path}"), 200),
      "no PASS {name} row:\n{stdout}"
    );
  }
  for skipped in [
    skip_endpoint(
      "codeload",
      "IP-literal host: no codeload subdomain exists (archives come from the host)",
    ),
    skip_endpoint("results", "not applicable on GHES (no Results service)"),
  ] {
    assert!(network.contains(&skipped.as_str()), "{skipped}\n{stdout}");
  }
  assert_eq!(last_line(&stdout), implied_summary(&stdout, 0), "{stdout}");
  assert!(!stdout.contains(TEST_BEARER), "{stdout}");
  Ok(())
}

#[test]
fn offline_makes_no_requests() -> TestResult {
  let home = tempfile::tempdir()?;
  let peer = Recorder::spawn(|head| Some(healthy_reply(head)))?;
  let base = format!("http://127.0.0.1:{}", peer.port());
  write_registration(home.path(), "o", "r", &base, false)?;

  let (_, stdout, stderr, _) = run(
    doctor_cmd(home.path())?
      .arg("--offline")
      .env("TOOLU_RUNNER_TOKEN", TEST_BEARER)
      .env(
        "TOOLU_RUNNER_RELEASES_URL",
        format!("{base}/releases/latest"),
      ),
  )?;
  assert!(peer.heads().is_empty(), "requests: {:?}", peer.heads());
  assert!(
    section(&stdout, "install").contains(&row("SKIP", "update", "offline").as_str()),
    "{}",
    report(&stdout, &stderr)
  );
  assert_eq!(
    section(&stdout, "github auth (127.0.0.1)"),
    [row(
      "SKIP",
      "token",
      "TOOLU_RUNNER_TOKEN: present, not verified (offline)"
    )],
    "{stdout}"
  );
  assert_eq!(
    network_rows(&stdout, "127.0.0.1"),
    [row("SKIP", "endpoints", "offline")],
    "{stdout}"
  );
  Ok(())
}

#[test]
fn endpoint_failure() -> TestResult {
  let home = tempfile::tempdir()?;
  let closed_port = closed_port()?;
  let closed = format!("http://127.0.0.1:{closed_port}");
  // Ephemeral, so no bearer is needed and only the network can fail.
  write_registration(home.path(), "o", "r", &closed, true)?;

  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.args(["--timeout", "5"]))?;
  assert_eq!(code, Some(1), "{}", report(&stdout, &stderr));
  let network = network_rows(&stdout, "127.0.0.1");
  // The four required probes with a URL fail; the two N/A ones are skipped.
  for (name, path) in [
    ("api", "/api/v3"),
    ("pipelines", "/pipelines/tenant/"),
    ("broker", "/broker/tenant/"),
    ("token", "/token/oauth"),
  ] {
    let frame = format!(
      "    FAIL  {name:<10} {closed}{path}  connect: error sending request for url ({closed}{path}): \
       client error (Connect): tcp connect error: Connection refused (os error "
    );
    let at = network
      .iter()
      .position(|line| line.starts_with(&frame) && line.ends_with(')'))
      .ok_or_else(|| format!("no FAIL {name} row:\n{stdout}"))?;
    assert_eq!(
      network.get(at + 1).copied(),
      Some("          hint: connection refused or unreachable: check firewall rules and the proxy"),
      "{stdout}"
    );
  }
  assert!(
    stdout.contains("result: 4 failures, "),
    "{}",
    report(&stdout, &stderr)
  );
  assert_eq!(last_line(&stdout), implied_summary(&stdout, 1), "{stdout}");
  Ok(())
}
