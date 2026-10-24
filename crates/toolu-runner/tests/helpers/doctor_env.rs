//! Shared fixtures for the `doctor` shell-out tests: a hermetic `toolu-runner
//! doctor` command, real registrations persisted the way `register` writes
//! them (built from the committed GHES JIT fixture), the captured GitHub
//! replies, and the row / section helpers the report assertions share.
//!
//! Hermetic means no test reaches the internet or the developer's own
//! credentials: `HOME` / `TOOLU_RUNNER_HOME` point at the test's tempdir, the
//! token / keyring / proxy / trust / Docker variables are removed, `https://`
//! traffic goes to a closed loopback proxy, and the release check targets a
//! closed loopback port unless a test supplies a peer.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

use config::auth_store::{AuthStore, StoredToken};
use config::config::{
  CacheSection, CredentialsFile, RunnerRegistrationConfig, RuntimeConfig, ServicesSection,
  ShadowSection, WorkspaceSection, save_config, save_credentials,
};

use crate::check_servers::{OK, Recorder, closed_port, jit_pointing_at};

/// Variables that would leak a developer's token, keyring choice, proxy,
/// trust store or Docker endpoint into a test.
const ISOLATED_VARS: [&str; 17] = [
  "TOOLU_RUNNER_TOKEN",
  "TOOLU_RUNNER_KEYRING",
  "TOOLU_RUNNER_NO_KEYRING",
  "TOOLU_RUNNER_RELEASES_URL",
  "HTTPS_PROXY",
  "https_proxy",
  "HTTP_PROXY",
  "http_proxy",
  "ALL_PROXY",
  "all_proxy",
  "NO_PROXY",
  "no_proxy",
  "REQUEST_METHOD",
  "SSL_CERT_FILE",
  "SSL_CERT_DIR",
  "NODE_EXTRA_CA_CERTS",
  "DOCKER_HOST",
];

/// `toolu-runner doctor` pinned to an isolated home and cwd.
pub fn doctor_cmd(home: &Path) -> std::io::Result<Command> {
  let mut cmd = Command::new(env!("CARGO_BIN_EXE_toolu-runner"));
  cmd
    .arg("doctor")
    .current_dir(home)
    .env("HOME", home)
    .env("TOOLU_RUNNER_HOME", home);
  for var in ISOLATED_VARS {
    cmd.env_remove(var);
  }
  cmd
    .env(
      "HTTPS_PROXY",
      format!("http://127.0.0.1:{}", closed_port()?),
    )
    .env("NO_PROXY", "127.0.0.1,localhost")
    .env(
      "TOOLU_RUNNER_RELEASES_URL",
      format!("http://127.0.0.1:{}/releases/latest", closed_port()?),
    );
  Ok(cmd)
}

/// The registration `register` would persist for `owner/repo` under
/// `<home>/runners/`: `runner_url` = `<base>/<owner>/<repo>` and the GHES JIT
/// fixture's service URLs pointed at `base`.
fn registration_config(
  home: &Path,
  owner: &str,
  repo: &str,
  base: &str,
  ephemeral: bool,
) -> Result<RunnerRegistrationConfig, Box<dyn std::error::Error>> {
  let dir = home.join("runners").join(owner).join(repo);
  Ok(RunnerRegistrationConfig {
    runner_url: format!("{base}/{owner}/{repo}"),
    runner_name: "doctor-test".to_owned(),
    runner_id: 7,
    auth_token: "client-id".to_owned(),
    labels: vec!["self-hosted".to_owned()],
    runner_group: "Default".to_owned(),
    ephemeral,
    runtime: RuntimeConfig {
      jit_config: jit_pointing_at(base)?,
      work_dir: dir.join("_work").to_string_lossy().into_owned(),
      data_dir: dir.to_string_lossy().into_owned(),
      protocol_version: "v1".to_owned(),
    },
    services: ServicesSection::default(),
    cache: CacheSection::default(),
    workspace: WorkspaceSection::default(),
    shadow: ShadowSection::default(),
    network: config::network::NetworkSection::default(),
    auth: config::registration_auth::AuthSection::default(),
    provider: config::provider::ProviderSection::Github,
  })
}

/// Persist a registration for `owner/repo` under `<home>/runners/` the way
/// `register` does (see [`registration_config`]), after letting `edit` change
/// the config — a broken JIT envelope, a `[network]` section. Returns its
/// `config.toml` path; `credentials.json` is written beside it, mode 0600.
pub fn write_registration_with(
  home: &Path,
  owner: &str,
  repo: &str,
  base: &str,
  ephemeral: bool,
  edit: impl FnOnce(&mut RunnerRegistrationConfig),
) -> Result<PathBuf, Box<dyn std::error::Error>> {
  let dir = home.join("runners").join(owner).join(repo);
  std::fs::create_dir_all(&dir)?;
  let config_path = dir.join("config.toml");
  let mut config = registration_config(home, owner, repo, base, ephemeral)?;
  edit(&mut config);
  save_config(&config_path, &config)?;
  save_credentials(
    &dir.join("credentials.json"),
    &CredentialsFile {
      access_token: "client-id".to_owned(),
      issued_at: "2026-10-19T00:00:00Z".to_owned(),
      expires_at: None,
    },
  )?;
  Ok(config_path)
}

/// [`write_registration_with`] without an edit.
pub fn write_registration(
  home: &Path,
  owner: &str,
  repo: &str,
  base: &str,
  ephemeral: bool,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
  write_registration_with(home, owner, repo, base, ephemeral, |_| {})
}

/// Persist a `login` token for `host` exactly as `toolu-runner login` does
/// with the default backend (`AuthStore::File`, a 0600 `token-<host>.json`
/// under the runner home). Returns the token file's path.
pub fn store_login(
  home: &Path,
  host: &str,
  token: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
  let store = AuthStore::File(home.to_path_buf());
  store.save(&StoredToken {
    access_token: token.to_owned(),
    scope: "repo".to_owned(),
    host: host.to_owned(),
    issued_at: "2026-10-19T00:00:00Z".to_owned(),
  })?;
  Ok(home.join(format!("token-{host}.json")))
}

/// Every path under `dir`, relative and sorted, with its mode, size, mtime
/// (nanoseconds) and — for files — a BLAKE3 digest of its bytes. A new file,
/// a rewritten body (even byte-identical, via the mtime), a `chmod` or a
/// touched directory all change the listing.
pub fn snapshot(dir: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
  let mut out = Vec::new();
  let mut stack = vec![dir.to_path_buf()];
  while let Some(next) = stack.pop() {
    for entry in std::fs::read_dir(&next)? {
      let path = entry?.path();
      let meta = std::fs::symlink_metadata(&path)?;
      let nanos = meta.modified()?.duration_since(UNIX_EPOCH)?.as_nanos();
      let digest = if meta.is_file() {
        blake3::hash(&std::fs::read(&path)?).to_string()
      } else {
        "-".to_owned()
      };
      out.push(format!(
        "{} mode={:o} size={} mtime={nanos} blake3={digest}",
        path.strip_prefix(dir)?.display(),
        meta.permissions().mode(),
        meta.len(),
      ));
      if meta.is_dir() {
        stack.push(path);
      }
    }
  }
  out.sort();
  Ok(out)
}

/// A static `application/json` reply: `$status`, `$headers` (each line ending
/// in `\r\n`, or empty) and `$body`.
macro_rules! json_reply {
  ($status:literal, $headers:literal, $body:expr) => {
    concat!(
      "HTTP/1.1 ",
      $status,
      "\r\ncontent-type: application/json\r\n",
      $headers,
      "connection: close\r\n\r\n",
      $body
    )
  };
}
pub(crate) use json_reply;

/// The bearer the env-token tests hand the binary; synthetic, never a
/// credential. A macro so reply bodies can `concat!` it.
macro_rules! test_bearer {
  () => {
    "doctor-test-bearer-0f3c9a"
  };
}

/// The stored `login` token; synthetic, never a credential.
macro_rules! stored_bearer {
  () => {
    "doctor-stored-login-7be1d2"
  };
}

pub const TEST_BEARER: &str = test_bearer!();
pub const STORED_BEARER: &str = stored_bearer!();

/// The captured real `GET …/releases/latest` body as an HTTP response.
pub const RELEASES_LATEST: &str = json_reply!(
  "200 OK",
  "",
  include_str!("../fixtures/doctor/releases_latest.json")
);

/// The captured real (sanitized) `GET /user` 200 body as an HTTP response.
pub const USER_200: &str = json_reply!(
  "200 OK",
  "",
  include_str!("../fixtures/doctor/github_user_200.json")
);

/// The same 200 with the `x-oauth-scopes` header a classic token earns.
pub const USER_200_SCOPES: &str = json_reply!(
  "200 OK",
  "x-oauth-scopes: repo, workflow\r\n",
  include_str!("../fixtures/doctor/github_user_200.json")
);

/// The same 200 with the empty `x-oauth-scopes` of a token that has none.
pub const USER_200_EMPTY_SCOPES: &str = json_reply!(
  "200 OK",
  "x-oauth-scopes: \r\n",
  include_str!("../fixtures/doctor/github_user_200.json")
);

/// The captured real `GET /user` 401 body as an HTTP response.
pub const USER_401: &str = json_reply!(
  "401 Unauthorized",
  "",
  include_str!("../fixtures/doctor/github_user_401.json")
);

/// A 401 whose `message` echoes `TEST_BEARER` — a misbehaving proxy.
pub const USER_401_ECHOES_ENV: &str = json_reply!(
  "401 Unauthorized",
  "",
  concat!(
    "{\"message\":\"Bad credentials for token ",
    test_bearer!(),
    "\"}"
  )
);

/// A 401 whose `message` echoes `STORED_BEARER`.
pub const USER_401_ECHOES_STORED: &str = json_reply!(
  "401 Unauthorized",
  "",
  concat!(
    "{\"message\":\"Bad credentials for token ",
    stored_bearer!(),
    "\"}"
  )
);

/// A 500 with an empty body.
pub const HTTP_500: &str =
  "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";

/// A peer answering `GET /api/v3/user` with `$user` and everything else (the
/// service endpoints, the release check) with a 200.
macro_rules! user_peer {
  ($user:expr) => {
    $crate::check_servers::Recorder::spawn(|head| {
      Some(if head.starts_with("GET /api/v3/user ") {
        $user
      } else {
        $crate::check_servers::OK
      })
    })
  };
}
pub(crate) use user_peer;

/// One loopback peer for every GitHub host a GHES-shaped registration names:
/// the release check gets the captured body, `/user` the captured 200, every
/// service endpoint a 200.
pub fn healthy_reply(head: &str) -> &'static str {
  if head.starts_with("GET /api/v3/user ") {
    USER_200
  } else if head.starts_with("GET /releases/latest ") {
    RELEASES_LATEST
  } else {
    OK
  }
}

/// `{ver}` — this build's version, which `version` and `update` rows print.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The `re-register` fix every unrepairable registration problem carries.
pub const REREGISTER: &str = "re-register: toolu-runner register --replace --url <repo url>";

/// A rendered row: `  <LEVEL>  <name padded to 10> <detail>`.
pub fn row(level: &str, name: &str, detail: &str) -> String {
  format!("  {level:<4}  {name:<10} {detail}")
}

/// A rendered `fix:` line.
pub fn fix(text: &str) -> String {
  format!("        fix: {text}")
}

/// The `fix:` line under a `service` row no unit (or more than one unit)
/// supervises: the machine service (#208).
pub fn service_install_fix() -> String {
  fix("toolu-runner install-service")
}

/// The `fix:` line under a registration's `run` row when it is not running.
pub fn install_fix(config: &Path) -> String {
  fix(&format!(
    "toolu-runner install-service --config {}",
    config.display()
  ))
}

/// Whether `lines` holds `expected` as consecutive lines.
pub fn has_lines(lines: &[&str], expected: &[String]) -> bool {
  !expected.is_empty()
    && lines.windows(expected.len()).any(|window| {
      window
        .iter()
        .zip(expected)
        .all(|(line, want)| *line == want.as_str())
    })
}

/// Why a rendered report is shown in an assertion message.
pub fn report(stdout: &str, stderr: &str) -> String {
  format!("stdout:\n{stdout}\nstderr:\n{stderr}")
}

/// The lines of the section titled `title`: its rows, `fix:` lines and
/// sub-headings, up to the next unindented line.
pub fn section<'a>(stdout: &'a str, title: &str) -> Vec<&'a str> {
  let header = format!("{title}:");
  stdout
    .lines()
    .skip_while(|line| *line != header)
    .skip(1)
    .take_while(|line| line.starts_with(' '))
    .collect()
}

/// The rows printed under the registration heading for `owner/repo`.
pub fn block<'a>(stdout: &'a str, owner_repo: &str) -> Vec<&'a str> {
  stdout
    .lines()
    .skip_while(|line| {
      !line.starts_with(&format!("  {owner_repo} (")) && *line != format!("  {owner_repo}:")
    })
    .skip(1)
    .take_while(|line| line.starts_with("  ") && !line.ends_with(':'))
    .collect()
}

/// The registration heading line `doctor` prints for a loaded registration.
pub fn heading(owner_repo: &str, mode: &str) -> String {
  format!("  {owner_repo} ({mode}, runner doctor-test):")
}

/// The rows of the `network (<host>)` section.
pub fn network_rows<'a>(stdout: &'a str, host: &str) -> Vec<&'a str> {
  section(stdout, &format!("network ({host})"))
}

/// A `SKIP` endpoint row inside the (indented) embedded `check` output.
pub fn skip_endpoint(name: &str, reason: &str) -> String {
  format!("    SKIP  {name:<10} {reason}")
}

/// Whether the embedded `check` output has the `PASS` row for `name` probing
/// `url` and answered with HTTP `status` in a whole number of milliseconds.
pub fn has_probe(lines: &[&str], name: &str, url: &str, status: u16) -> bool {
  let prefix = format!("    PASS  {name:<10} {url}  HTTP {status}  ");
  lines.iter().any(|line| {
    line
      .strip_prefix(prefix.as_str())
      .and_then(|rest| rest.strip_suffix(" ms"))
      .is_some_and(|ms| !ms.is_empty() && ms.bytes().all(|byte| byte.is_ascii_digit()))
  })
}

/// The `result:` line the rendered rows imply: every `FAIL` / `WARN` row —
/// `doctor`'s own and the ones embedded from `check` — is counted once.
pub fn implied_summary(stdout: &str, code: i32) -> String {
  let count = |tag: &str| {
    stdout
      .lines()
      .filter(|line| line.trim_start().starts_with(&format!("{tag}  ")))
      .count()
  };
  let noun = |n: usize, word: &str| {
    if n == 1 {
      format!("{n} {word}")
    } else {
      format!("{n} {word}s")
    }
  };
  format!(
    "result: {}, {} (exit {code})",
    noun(count("FAIL"), "failure"),
    noun(count("WARN"), "warning")
  )
}

/// The report's last line.
pub fn last_line(stdout: &str) -> &str {
  stdout.lines().last().unwrap_or_default()
}

/// The `authorization` header line of the one `GET /api/v3/user` `peer` saw,
/// lower-cased (the synthetic tokens are lower-case already).
pub fn user_authorization(peer: &Recorder) -> Result<String, Box<dyn std::error::Error>> {
  let head = peer
    .heads()
    .into_iter()
    .find(|head| head.starts_with("GET /api/v3/user "))
    .ok_or("doctor never asked GET /api/v3/user")?;
  head
    .lines()
    .map(str::to_ascii_lowercase)
    .find(|line| line.starts_with("authorization:"))
    .ok_or_else(|| format!("no authorization header in:\n{head}").into())
}

/// A PID that belonged to a real process which has exited and been reaped.
pub fn dead_pid() -> Result<u32, Box<dyn std::error::Error>> {
  for _ in 0..5 {
    let mut child = std::process::Command::new("true").spawn()?;
    let pid = child.id();
    child.wait()?;
    if !config::lockfile::is_pid_alive(pid) {
      return Ok(pid);
    }
  }
  Err("every reaped PID was reused at once".into())
}

/// Write a `.lock` body the way `run` does, naming `pid`; returns the
/// `started_at` it recorded.
pub fn write_lock_body(reg_dir: &Path, pid: u32, config: &Path) -> std::io::Result<String> {
  let started_at = chrono::Utc::now().to_rfc3339();
  let body = config::lockfile::LockBody {
    pid,
    started_at: started_at.clone(),
    config_path: config.display().to_string(),
  };
  std::fs::write(
    reg_dir.join(".lock"),
    serde_json::to_string(&body).map_err(std::io::Error::other)?,
  )?;
  Ok(started_at)
}

/// The `jit` row for the registration at `config`: the fixture envelope
/// parses, and the row names its length.
pub fn jit_ok(config: &Path) -> Result<String, Box<dyn std::error::Error>> {
  let loaded = ::config::config::load_config(config)?;
  Ok(row(
    "OK",
    "jit",
    &format!("parses ({} bytes)", loaded.runtime.jit_config.len()),
  ))
}

/// The fix for a rejected `TOOLU_RUNNER_TOKEN` against the loopback host.
pub const ENV_REJECTED_FIX: &str =
  "export a valid TOOLU_RUNNER_TOKEN, or unset it and run `toolu-runner login 127.0.0.1`";
