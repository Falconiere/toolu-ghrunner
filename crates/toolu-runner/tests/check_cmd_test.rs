//! Shell-out tests for `toolu-runner check` (#93): the compiled binary run
//! against real loopback peers — HTTP targets, forward / CONNECT / 407
//! proxies, a peer that never answers — plus a real registration built
//! from the committed GHES JIT fixture and the public ISRG Root X1 CA.

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use config::config::{
  CacheSection, CredentialsFile, RunnerRegistrationConfig, RuntimeConfig, ServicesSection,
  ShadowSection, WorkspaceSection, save_config, save_credentials,
};
use fs2::FileExt;

use crate::check_servers;

use check_servers::{OK, Recorder, check_cmd, closed_port, jit_pointing_at, run};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const ISRG_ROOT: &str = "tests/fixtures/check/isrg-root-x1.pem";

/// Sorted entry names; an unreadable directory is an error, never "empty".
fn listing(dir: &Path) -> std::io::Result<Vec<String>> {
  let mut names = Vec::new();
  for entry in std::fs::read_dir(dir)? {
    names.push(entry?.file_name().to_string_lossy().into_owned());
  }
  names.sort();
  Ok(names)
}

#[test]
fn healthy_ghes_target_passes_and_stays_read_only() -> TestResult {
  let home = tempfile::tempdir()?;
  let target = Recorder::spawn(|_| Some(OK))?;
  let base = format!("http://127.0.0.1:{}", target.port());
  let (code, stdout, stderr, _) =
    run(check_cmd(home.path())?.args(["--url", &base, "--timeout", "5"]))?;

  assert_eq!(code, Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
  assert!(
    stdout.contains(&format!("  PASS  api        {base}/api/v3  HTTP 200")),
    "{stdout}"
  );
  assert!(
    stdout.contains(&format!(
      "  PASS  pipelines  {base}/_services/pipelines/_apis/health  HTTP 200"
    )),
    "{stdout}"
  );
  assert!(
    stdout.contains(&format!(
      "  PASS  token      {base}/_services/vstoken/_apis/health  HTTP 200"
    )),
    "{stdout}"
  );
  assert!(
    stdout.contains("  SKIP  codeload   IP-literal host"),
    "{stdout}"
  );
  assert!(
    stdout.contains("  SKIP  broker     GHES uses the V1 protocol"),
    "{stdout}"
  );
  assert!(
    stdout.contains("  SKIP  results    not applicable on GHES"),
    "{stdout}"
  );
  assert!(stdout.contains("target 127.0.0.1 (--url)"), "{stdout}");
  let mut lines = target.request_lines();
  lines.sort();
  assert_eq!(
    lines,
    [
      "GET /_services/pipelines/_apis/health HTTP/1.1",
      "GET /_services/vstoken/_apis/health HTTP/1.1",
      "GET /api/v3 HTTP/1.1",
    ]
  );
  let entries = listing(home.path())?;
  assert!(entries.is_empty(), "check wrote into the home: {entries:?}");
  Ok(())
}

/// Persist a registration under `<home>/runners/o/r/` the way `register` does.
fn write_registration(
  home: &Path,
  runner_url: &str,
  jit: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
  let dir = home.join("runners").join("o").join("r");
  std::fs::create_dir_all(&dir)?;
  let config_path = dir.join("config.toml");
  save_config(
    &config_path,
    &RunnerRegistrationConfig {
      runner_url: runner_url.to_owned(),
      runner_name: "check-test".to_owned(),
      runner_id: 7,
      auth_token: "client-id".to_owned(),
      labels: vec!["self-hosted".to_owned()],
      runner_group: "Default".to_owned(),
      ephemeral: false,
      runtime: RuntimeConfig {
        jit_config: jit.to_owned(),
        work_dir: dir.join("_work").to_string_lossy().into_owned(),
        data_dir: dir.to_string_lossy().into_owned(),
        protocol_version: "v1".to_owned(),
      },
      services: ServicesSection::default(),
      cache: CacheSection::default(),
      workspace: WorkspaceSection::default(),
      shadow: ShadowSection::default(),
      network: config::network::NetworkSection::default(),
    },
  )?;
  save_credentials(
    &dir.join("credentials.json"),
    &CredentialsFile {
      access_token: "client-id".to_owned(),
      issued_at: "2026-10-12T00:00:00Z".to_owned(),
      expires_at: None,
    },
  )?;
  Ok(config_path)
}

#[test]
fn configured_registration_endpoints_are_probed_while_run_holds_the_lock() -> TestResult {
  let home = tempfile::tempdir()?;
  let target = Recorder::spawn(|_| Some(OK))?;
  let base = format!("http://127.0.0.1:{}", target.port());
  let config_path = write_registration(
    home.path(),
    &format!("{base}/my-org"),
    &jit_pointing_at(&base)?,
  )?;
  let reg_dir = config_path.parent().ok_or("no parent")?;
  let lock = std::fs::File::create(reg_dir.join(".lock"))?;
  lock.try_lock_exclusive()?;
  let before = listing(reg_dir)?;

  let (code, stdout, stderr, _) = run(check_cmd(home.path())?.args(["--timeout", "5"]))?;

  assert_eq!(code, Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
  assert!(
    stdout.contains(&format!("(registration: {})", config_path.display())),
    "{stdout}"
  );
  assert!(
    stdout.contains(&format!(
      "  PASS  broker     {base}/broker/tenant/  HTTP 200"
    )),
    "{stdout}"
  );
  assert!(stdout.contains("cached runtimes: none"), "{stdout}");
  let mut lines = target.request_lines();
  lines.sort();
  assert_eq!(
    lines,
    [
      "GET /api/v3 HTTP/1.1",
      "GET /broker/tenant/ HTTP/1.1",
      "GET /pipelines/tenant/ HTTP/1.1",
      "GET /token/oauth HTTP/1.1",
    ]
  );
  assert_eq!(
    listing(reg_dir)?,
    before,
    "check changed the registration dir"
  );
  FileExt::unlock(&lock)?;
  Ok(())
}

#[test]
fn hanging_endpoint_is_bounded_by_the_timeout() -> TestResult {
  let home = tempfile::tempdir()?;
  let silent = Recorder::spawn(|_| None)?;
  let base = format!("http://127.0.0.1:{}", silent.port());
  let (code, stdout, _, elapsed) =
    run(check_cmd(home.path())?.args(["--url", &base, "--timeout", "1"]))?;

  assert_eq!(code, Some(1), "{stdout}");
  assert!(
    stdout.contains(&format!("  FAIL  api        {base}/api/v3  timeout:")),
    "{stdout}"
  );
  assert!(
    stdout.contains("hint: no response within the bound"),
    "{stdout}"
  );
  assert!(elapsed < Duration::from_secs(10), "took {elapsed:?}");
  Ok(())
}

/// A peer that accepts, lets the request arrive, then closes without reading
/// it — the kernel answers unread data with a TCP RST.
fn resetting_peer() -> std::io::Result<u16> {
  let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
  let port = listener.local_addr()?.port();
  std::thread::spawn(move || {
    for stream in listener.incoming().flatten() {
      std::thread::sleep(Duration::from_millis(300));
      drop(stream);
    }
  });
  Ok(port)
}

#[test]
fn a_connection_reset_is_classified_as_connect_not_tls() -> TestResult {
  let home = tempfile::tempdir()?;
  let base = format!("http://127.0.0.1:{}", resetting_peer()?);
  let (code, stdout, _, _) = run(check_cmd(home.path())?.args(["--url", &base, "--timeout", "5"]))?;
  assert_eq!(code, Some(1), "{stdout}");
  assert!(
    stdout.contains(&format!("  FAIL  api        {base}/api/v3  connect:")),
    "{stdout}"
  );
  assert!(!stdout.contains("tls:"), "{stdout}");
  Ok(())
}

#[test]
fn dns_refused_and_tls_failures_are_classified() -> TestResult {
  let home = tempfile::tempdir()?;
  let (code, stdout, _, _) = run(
    check_cmd(home.path())?
      .env("NO_PROXY", "127.0.0.1,toolu-check.invalid")
      .args(["--url", "https://toolu-check.invalid", "--timeout", "5"]),
  )?;
  assert_eq!(code, Some(1), "{stdout}");
  assert!(
    stdout.contains("  FAIL  api        https://toolu-check.invalid/api/v3  dns:"),
    "{stdout}"
  );

  let refused = format!("http://127.0.0.1:{}", closed_port()?);
  let (code, stdout, _, _) =
    run(check_cmd(home.path())?.args(["--url", &refused, "--timeout", "5"]))?;
  assert_eq!(code, Some(1), "{stdout}");
  assert!(
    stdout.contains(&format!("  FAIL  api        {refused}/api/v3  connect:")),
    "{stdout}"
  );

  let plaintext = Recorder::spawn(|_| Some(OK))?;
  let tls = format!("https://127.0.0.1:{}", plaintext.port());
  let (code, stdout, _, _) = run(check_cmd(home.path())?.args(["--url", &tls, "--timeout", "5"]))?;
  assert_eq!(code, Some(1), "{stdout}");
  assert!(
    stdout.contains(&format!("  FAIL  api        {tls}/api/v3  tls:")),
    "{stdout}"
  );
  Ok(())
}

/// What a proxy test observed.
struct ProxyRun {
  code: Option<i32>,
  stdout: String,
  stderr: String,
  target: Recorder,
  proxy: Recorder,
}

/// Common setup for the proxy tests: a target that must stay silent when
/// the proxy is used, and a proxy that answers with `respond`.
fn proxy_run(
  respond: check_servers::Respond,
  no_proxy: &str,
  extra: &[(&str, &str)],
) -> Result<ProxyRun, Box<dyn std::error::Error>> {
  let home = tempfile::tempdir()?;
  let target = Recorder::spawn(|_| Some(OK))?;
  let proxy = Recorder::spawn(respond)?;
  let mut cmd = check_cmd(home.path())?;
  cmd
    .env(
      "HTTP_PROXY",
      format!("http://user:S3cr3tPw-93@127.0.0.1:{}", proxy.port()),
    )
    .env("NO_PROXY", no_proxy)
    .args([
      "--url",
      &format!("http://127.0.0.1:{}", target.port()),
      "--timeout",
      "5",
    ]);
  for (key, value) in extra {
    cmd.env(key, value);
  }
  let (code, stdout, stderr, _) = run(&mut cmd)?;
  Ok(ProxyRun {
    code,
    stdout,
    stderr,
    target,
    proxy,
  })
}

#[test]
fn authenticated_proxy_is_used_and_redacted() -> TestResult {
  let ProxyRun {
    code,
    stdout,
    stderr,
    target,
    proxy,
  } = proxy_run(|_| Some(OK), "", &[])?;
  assert_eq!(code, Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
  assert!(
    target.heads().is_empty(),
    "target was reached directly: {:?}",
    target.heads()
  );
  let expected_line = format!("GET http://127.0.0.1:{}/api/v3 HTTP/1.1", target.port());
  assert!(
    proxy.request_lines().contains(&expected_line),
    "{:?}",
    proxy.request_lines()
  );
  let basic = format!(
    "proxy-authorization: basic {}",
    B64.encode("user:S3cr3tPw-93")
  )
  .to_lowercase();
  assert!(
    proxy
      .heads()
      .iter()
      .all(|head| head.to_lowercase().contains(&basic)),
    "{:?}",
    proxy.heads()
  );
  assert!(
    stdout.contains(&format!(
      "HTTP_PROXY   http://***@127.0.0.1:{}",
      proxy.port()
    )),
    "{stdout}"
  );
  assert!(
    stdout.contains("routing      https:// via HTTPS_PROXY; http:// via HTTP_PROXY"),
    "{stdout}"
  );
  assert!(!stdout.contains("S3cr3tPw-93") && !stderr.contains("S3cr3tPw-93"));
  Ok(())
}

#[test]
fn no_proxy_bypasses_the_proxy() -> TestResult {
  let ProxyRun {
    code,
    stdout,
    target,
    proxy,
    ..
  } = proxy_run(|_| Some(OK), "127.0.0.1", &[])?;
  assert_eq!(code, Some(0), "{stdout}");
  assert!(
    proxy.heads().is_empty(),
    "proxy was used: {:?}",
    proxy.heads()
  );
  assert_eq!(
    target.request_lines().len(),
    3,
    "{:?}",
    target.request_lines()
  );
  Ok(())
}

#[test]
fn proxy_407_fails_the_check() -> TestResult {
  let ProxyRun {
    code,
    stdout,
    stderr,
    ..
  } = proxy_run(
    |_| {
      Some(
        "HTTP/1.1 407 Proxy Authentication Required\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
      )
    },
    "",
    &[],
  )?;
  assert_eq!(code, Some(1), "{stdout}");
  assert!(
    stdout.contains("proxy: HTTP 407 Proxy Authentication Required"),
    "{stdout}"
  );
  assert!(!stdout.contains("S3cr3tPw-93") && !stderr.contains("S3cr3tPw-93"));
  Ok(())
}

#[test]
fn cgi_guard_disables_the_proxy_and_is_reported() -> TestResult {
  let ProxyRun {
    code,
    stdout,
    target,
    proxy,
    ..
  } = proxy_run(|_| Some(OK), "", &[("REQUEST_METHOD", "GET")])?;
  assert_eq!(code, Some(0), "{stdout}");
  assert!(
    stdout.contains("REQUEST_METHOD is set: every proxy variable is ignored"),
    "{stdout}"
  );
  assert!(
    proxy.heads().is_empty(),
    "proxy was used: {:?}",
    proxy.heads()
  );
  assert_eq!(
    target.request_lines().len(),
    3,
    "{:?}",
    target.request_lines()
  );
  Ok(())
}

#[test]
fn ssl_cert_file_and_node_extra_ca_certs_are_reported() -> TestResult {
  let home = tempfile::tempdir()?;
  let target = Recorder::spawn(|_| Some(OK))?;
  let base = format!("http://127.0.0.1:{}", target.port());
  let pem = Path::new(env!("CARGO_MANIFEST_DIR")).join(ISRG_ROOT);
  let (code, stdout, _, _) = run(
    check_cmd(home.path())?
      .env("SSL_CERT_FILE", &pem)
      .env("NODE_EXTRA_CA_CERTS", &pem)
      .args(["--url", &base, "--timeout", "5"]),
  )?;
  assert_eq!(code, Some(0), "{stdout}");
  assert!(
    stdout.contains(&format!(
      "SSL_CERT_FILE        {} (replaces the platform store)",
      pem.display()
    )),
    "{stdout}"
  );
  assert!(
    stdout.contains("anchors              1 loaded, 0 errors"),
    "{stdout}"
  );
  assert!(
    stdout.contains(&format!("NODE_EXTRA_CA_CERTS  {}", pem.display())),
    "{stdout}"
  );

  let missing = home.path().join("absent.pem");
  let (_, stdout, _, _) = run(
    check_cmd(home.path())?
      .env("SSL_CERT_FILE", &missing)
      .args(["--url", &base, "--timeout", "5"]),
  )?;
  assert!(
    stdout.contains("0 loaded, 1 errors — only the bundled roots remain"),
    "{stdout}"
  );
  assert!(stdout.contains("load error"), "{stdout}");
  Ok(())
}

#[test]
fn usage_and_config_errors_exit_2() -> TestResult {
  let home = tempfile::tempdir()?;
  for args in [
    vec!["--url", "ftp://github.com"],
    vec!["--timeout", "0"],
    vec!["--config", "/nonexistent/toolu/config.toml"],
    vec!["--url", "https://github.com", "--config", "/x/config.toml"],
  ] {
    let (code, stdout, stderr, _) = run(check_cmd(home.path())?.args(&args))?;
    assert_eq!(
      code,
      Some(2),
      "{args:?}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.is_empty(), "{args:?} probed anyway: {stdout}");
  }
  let config_path = write_registration(home.path(), "https://github.com/o/r", "s3cr3t-garbage!!")?;
  let (code, stdout, stderr, _) = run(check_cmd(home.path())?.arg("--config").arg(&config_path))?;
  assert_eq!(code, Some(2), "stdout:\n{stdout}\nstderr:\n{stderr}");
  // The whole output, exactly: nothing derived from the value (substring,
  // base64, escaped form) can appear when nothing else is printed.
  assert_eq!(
    stderr,
    "toolu-runner: the registration's jit_config is not a valid JIT config envelope\n"
  );
  assert!(stdout.is_empty(), "{stdout}");
  Ok(())
}

#[test]
fn unauthenticated_401_is_reachable() -> TestResult {
  let home = tempfile::tempdir()?;
  let target = Recorder::spawn(|head| {
    Some(if head.starts_with("GET /_services/vstoken/") {
      "HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
    } else {
      OK
    })
  })?;
  let base = format!("http://127.0.0.1:{}", target.port());
  let (code, stdout, _, _) = run(check_cmd(home.path())?.args(["--url", &base, "--timeout", "5"]))?;
  assert_eq!(code, Some(0), "{stdout}");
  assert!(
    stdout.contains(&format!(
      "  PASS  token      {base}/_services/vstoken/_apis/health  HTTP 401"
    )),
    "{stdout}"
  );
  Ok(())
}

#[test]
fn defaults_target_the_github_com_hosts() -> TestResult {
  let home = tempfile::tempdir()?;
  let proxy = Recorder::spawn(|_| {
    Some("HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
  })?;
  let (code, stdout, stderr, _) = run(
    check_cmd(home.path())?
      .env("HTTPS_PROXY", format!("http://127.0.0.1:{}", proxy.port()))
      .env_remove("NO_PROXY")
      .args(["--timeout", "5"]),
  )?;

  assert_eq!(code, Some(1), "stdout:\n{stdout}\nstderr:\n{stderr}");
  assert!(
    stdout.contains("target github.com (defaults: config error: no runner registration found"),
    "{stdout}"
  );
  assert!(
    stdout.contains("result: 6 required checks failed (exit 1)"),
    "{stdout}"
  );
  let mut lines = proxy.request_lines();
  lines.sort();
  assert_eq!(
    lines,
    [
      "CONNECT api.github.com:443 HTTP/1.1",
      "CONNECT broker.actions.githubusercontent.com:443 HTTP/1.1",
      "CONNECT codeload.github.com:443 HTTP/1.1",
      "CONNECT nodejs.org:443 HTTP/1.1",
      "CONNECT pipelines.actions.githubusercontent.com:443 HTTP/1.1",
      "CONNECT results-receiver.actions.githubusercontent.com:443 HTTP/1.1",
      "CONNECT vstoken.actions.githubusercontent.com:443 HTTP/1.1",
    ]
  );
  let entries = listing(home.path())?;
  assert!(entries.is_empty(), "{entries:?}");
  Ok(())
}

/// AC-10 — real github.com from a networked host. Run manually:
/// `cargo test -p toolu-runner --features live --test integration -- --ignored check_cmd_test::live_github_com`.
#[cfg(feature = "live")]
#[test]
#[ignore = "live: needs outbound access to github.com"]
fn live_github_com() -> TestResult {
  let home = tempfile::tempdir()?;
  let mut cmd = check_cmd(home.path())?;
  cmd.env_remove("HTTPS_PROXY").env_remove("NO_PROXY").args([
    "--url",
    "https://github.com",
    "--timeout",
    "20",
  ]);
  let (code, stdout, _, _) = run(&mut cmd)?;
  println!("{stdout}");
  assert_eq!(code, Some(0), "{stdout}");
  for name in ["api", "codeload", "pipelines", "broker", "token", "results"] {
    assert!(
      stdout.contains(&format!("  PASS  {name:<10} https://")),
      "{name}: {stdout}"
    );
  }

  let pem = Path::new(env!("CARGO_MANIFEST_DIR")).join(ISRG_ROOT);
  let mut narrowed = check_cmd(home.path())?;
  narrowed
    .env_remove("HTTPS_PROXY")
    .env_remove("NO_PROXY")
    .env("SSL_CERT_FILE", &pem)
    .args(["--url", "https://github.com", "--timeout", "20"]);
  let (code, stdout, _, _) = run(&mut narrowed)?;
  println!("{stdout}");
  // The compiled-in Mozilla roots stay trusted, so an ISRG-only bundle cannot
  // narrow trust: github.com (not ISRG-issued) still verifies, and the
  // report says why.
  assert_eq!(code, Some(0), "{stdout}");
  assert!(
    stdout.contains("anchors              1 loaded, 0 errors"),
    "{stdout}"
  );
  assert!(
    stdout.contains("bundled              Mozilla root set compiled in"),
    "{stdout}"
  );
  Ok(())
}
