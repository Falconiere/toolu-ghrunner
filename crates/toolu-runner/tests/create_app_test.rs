//! Shell-out tests for `toolu-runner create-app`.
//!
//! The guard cases exercise the checks that must fire BEFORE any socket
//! bind, browser launch, or network call. The #202 cases drive the whole
//! flow through the real binary: `TOOLU_RUNNER_GITHUB_API_URL` points at a
//! loopback peer replaying GitHub's replies (live captures and documented
//! examples in `fixtures/github_app/`, the conversion fixture with the
//! protocol test key), the browser is replaced by HTTP GETs of the served
//! form and of GitHub's callback redirect, and every App call must carry a
//! JWT that verifies with the test key's public half. `TOOLU_RUNNER_HOME` is
//! a fresh temp dir per test; stdin is null, so every run is unattended.

use std::process::Command;

/// AC-8: an unsupported `--host` is rejected before any network / browser.
#[test]
fn create_app_rejects_non_github_host() {
  let home = tempfile::tempdir().expect("tempdir");
  let output = Command::new(env!("CARGO_BIN_EXE_toolu-runner"))
    .env("TOOLU_RUNNER_HOME", home.path())
    .args(["create-app", "--host", "ghes.example.com"])
    .output()
    .expect("should run toolu-runner create-app");

  assert!(
    !output.status.success(),
    "create-app against a non-github.com host must fail"
  );
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("is not supported yet") && stderr.contains("github.com only this release"),
    "error should be the host guard naming github.com as the only supported host: {stderr}"
  );
  assert!(
    stderr.contains("ghes.example.com"),
    "error should name the rejected host: {stderr}"
  );
}

/// AC-7: an existing App file blocks creation without `--force`, and the
/// guard fires before the callback server binds — so the process exits
/// immediately AND the existing file is left byte-for-byte untouched.
#[test]
fn create_app_refuses_to_overwrite_without_force() {
  let home = tempfile::tempdir().expect("tempdir");
  let app_path = home.path().join("github-app.json");
  let original = b"not valid json \x00\x01 arbitrary bytes".to_vec();
  std::fs::write(&app_path, &original).expect("write pre-existing app file");

  let output = Command::new(env!("CARGO_BIN_EXE_toolu-runner"))
    .env("TOOLU_RUNNER_HOME", home.path())
    .arg("create-app")
    .output()
    .expect("should run toolu-runner create-app");

  assert!(
    !output.status.success(),
    "create-app must refuse to overwrite an existing App without --force"
  );
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("pass --force to overwrite it"),
    "error should name --force as the override: {stderr}"
  );

  let after = std::fs::read(&app_path).expect("read app file back");
  assert_eq!(
    after, original,
    "the guard must not modify the existing App file"
  );
}

/// `create-app --help` parses and exits cleanly, and documents its flags.
#[test]
fn create_app_help_lists_flags() {
  let output = Command::new(env!("CARGO_BIN_EXE_toolu-runner"))
    .args(["create-app", "--help"])
    .output()
    .expect("should run toolu-runner create-app --help");

  assert!(
    output.status.success(),
    "create-app --help should exit cleanly: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  let stdout = String::from_utf8_lossy(&output.stdout);
  for flag in [
    "--name <NAME>",
    "--org <ORG>",
    "--account <LOGIN>",
    "--import <FILE>",
    "--app-id <ID>",
    "--app-key <PEM_FILE>",
    "--host <HOST>",
    "--no-browser",
    "--force",
  ] {
    assert!(stdout.contains(flag), "missing {flag} in: {stdout}");
  }
}

// ── #202: org-owned Apps, the installation wait, --import ─────────────

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use crate::github_app_fixtures::{
  APP_ID, AppJwtGate, EchoAuthorization, PRIVATE_PEM, capture, edited_reply, set_json,
  verify_app_jwt, write_app,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const CONVERSION: &str = include_str!("../../protocol/tests/fixtures/conversion_response.json");
const CODE: &str = "code202";

/// The conversion fixture as GitHub returns it for an App `owner` (id
/// `owner_id`) owns, carrying the test key.
fn conversion(owner: &str, owner_id: i64) -> TestResult<ResponseTemplate> {
  let mut body: Value = serde_json::from_str(CONVERSION)?;
  set_json(&mut body, "/id", json!(APP_ID))?;
  set_json(&mut body, "/slug", json!(format!("toolu-runner-{owner}")))?;
  set_json(&mut body, "/owner", json!({"login": owner, "id": owner_id}))?;
  set_json(&mut body, "/pem", json!(PRIVATE_PEM))?;
  Ok(ResponseTemplate::new(201).set_body_json(body))
}

/// The documented installation list with its item on `account`, `selection`.
fn installed_on(account: &str, selection: &str) -> TestResult<ResponseTemplate> {
  let (account, selection) = (account.to_owned(), selection.to_owned());
  edited_reply("installations_200.json", move |list| {
    set_json(list, "/0/account/login", json!(account))?;
    set_json(list, "/0/repository_selection", json!(selection))
  })
}

/// The first installation poll: answers `[]` only if `app_file` is already
/// saved 0600 (so the App was written before the wait began) and the JWT
/// verifies; otherwise a 418 the runner treats as fatal.
struct SavedBeforePoll(PathBuf);

impl Respond for SavedBeforePoll {
  fn respond(&self, request: &Request) -> ResponseTemplate {
    use std::os::unix::fs::PermissionsExt as _;
    let saved = std::fs::metadata(&self.0).is_ok_and(|m| m.permissions().mode() & 0o777 == 0o600);
    let signed = request
      .headers
      .get("authorization")
      .and_then(|v| v.to_str().ok())
      .is_some_and(|v| verify_app_jwt(v).is_ok());
    if saved && signed {
      ResponseTemplate::new(200).set_body_json(json!([]))
    } else {
      ResponseTemplate::new(418).set_body_string(format!("saved={saved} signed={signed}"))
    }
  }
}

/// Mount `GET /app/installations`: one empty poll (checked by
/// [`SavedBeforePoll`]), then `then`.
async fn mount_installations(server: &MockServer, home: &Path, then: ResponseTemplate) {
  Mock::given(method("GET"))
    .and(path("/app/installations"))
    .respond_with(SavedBeforePoll(home.join("github-app.json")))
    .up_to_n_times(1)
    .mount(server)
    .await;
  Mock::given(method("GET"))
    .and(path("/app/installations"))
    .respond_with(AppJwtGate(then))
    .mount(server)
    .await;
}

/// `create-app <args>` against `server`, unattended, waiting `wait_ms`.
fn create_app(
  home: &Path,
  server: &MockServer,
  args: &[&str],
  wait_ms: u64,
) -> tokio::process::Command {
  let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_toolu-runner"));
  cmd
    .env("TOOLU_RUNNER_HOME", home)
    .env("HOME", home)
    .env("TOOLU_RUNNER_GITHUB_API_URL", server.uri())
    .env("TOOLU_RUNNER_INSTALL_POLL_MS", "50")
    .env("TOOLU_RUNNER_INSTALL_WAIT_MS", wait_ms.to_string())
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true)
    .arg("create-app")
    .arg("--no-browser")
    .args(args);
  cmd
}

/// What a finished run printed and how it exited.
struct Finished {
  code: Option<i32>,
  stdout: String,
  stderr: String,
  /// The manifest form the binary served (create mode only).
  form: String,
}

/// Run `cmd`; in create mode play the browser: fetch the served form, then
/// follow GitHub's redirect to `/callback` with [`CODE`] and the form's state.
async fn run(mut cmd: tokio::process::Command, browser: bool) -> TestResult<Finished> {
  let mut child = cmd.spawn()?;
  let mut stdout = BufReader::new(child.stdout.take().ok_or("no stdout")?);
  let mut seen = String::new();
  let mut form = String::new();
  if browser {
    let local = loop {
      let mut line = String::new();
      if stdout.read_line(&mut line).await? == 0 {
        break None;
      }
      seen.push_str(&line);
      if let Some(url) = line.trim().strip_prefix("Create the App in your browser: ") {
        break Some(url.to_owned());
      }
    };
    if let Some(local) = local {
      let client = reqwest::Client::new();
      form = client.get(&local).send().await?.text().await?;
      let state = form
        .split_once("?state=")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(state, _)| state.to_owned())
        .ok_or("served form has no state")?;
      client
        .get(format!("{local}callback?code={CODE}&state={state}"))
        .send()
        .await?;
    }
  }
  let mut rest = String::new();
  tokio::time::timeout(Duration::from_secs(60), stdout.read_to_string(&mut rest)).await??;
  seen.push_str(&rest);
  let output = tokio::time::timeout(Duration::from_secs(60), child.wait_with_output()).await??;
  Ok(Finished {
    code: output.status.code(),
    stdout: seen,
    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    form,
  })
}

/// The saved `github-app.json` under `home`.
fn saved(home: &Path) -> TestResult<Value> {
  Ok(serde_json::from_str(&std::fs::read_to_string(
    home.join("github-app.json"),
  )?)?)
}

/// The manifest JSON embedded in the served form.
fn served_manifest(form: &str) -> TestResult<Value> {
  let attr = form
    .split_once("name=\"manifest\" value=\"")
    .and_then(|(_, rest)| rest.split_once('"'))
    .map(|(value, _)| value)
    .ok_or("form has no manifest")?;
  Ok(serde_json::from_str(&html_unescape(attr)?)?)
}

/// Decode an HTML attribute value: the named entities `&amp; &quot; &apos;
/// &lt; &gt;` and every decimal (`&#39;`) or hex (`&#x27;`) character
/// reference. An unknown or unterminated entity is an error, not passed on.
fn html_unescape(text: &str) -> TestResult<String> {
  let mut out = String::with_capacity(text.len());
  let mut rest = text;
  while let Some((before, after)) = rest.split_once('&') {
    out.push_str(before);
    let (entity, tail) = after
      .split_once(';')
      .ok_or_else(|| format!("unterminated entity in {text:?}"))?;
    let decoded = match entity {
      "amp" => '&',
      "quot" => '"',
      "apos" => '\'',
      "lt" => '<',
      "gt" => '>',
      _ => {
        let code = match entity
          .strip_prefix("#x")
          .or_else(|| entity.strip_prefix("#X"))
        {
          Some(hex) => u32::from_str_radix(hex, 16)?,
          None => entity
            .strip_prefix('#')
            .ok_or_else(|| format!("unknown entity &{entity}; in {text:?}"))?
            .parse()?,
        };
        char::from_u32(code).ok_or_else(|| format!("invalid character reference &{entity};"))?
      },
    };
    out.push(decoded);
    rest = tail;
  }
  out.push_str(rest);
  Ok(out)
}

/// AC-2 / S2: an org App is created through the org form with the org
/// permission, saved 0600 before the first poll, and its installation is
/// recorded after one empty poll.
#[tokio::test]
async fn org_create_waits_then_records_installation() -> TestResult {
  let home = tempfile::tempdir()?;
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/orgs/acme"))
    .respond_with(edited_reply("orgs_200.json", |org| {
      set_json(org, "/login", json!("acme"))?;
      set_json(org, "/id", json!(7))
    })?)
    .expect(1)
    .mount(&server)
    .await;
  Mock::given(method("POST"))
    .and(path(format!("/app-manifests/{CODE}/conversions")))
    .respond_with(conversion("acme", 7)?)
    .expect(1)
    .mount(&server)
    .await;
  mount_installations(&server, home.path(), installed_on("acme", "all")?).await;

  let done = run(
    create_app(home.path(), &server, &["--org", "acme"], 60_000),
    true,
  )
  .await?;
  assert_eq!(
    done.code,
    Some(0),
    "stdout:\n{}\nstderr:\n{}",
    done.stdout,
    done.stderr
  );
  assert!(
    done
      .form
      .contains("action=\"https://github.com/organizations/acme/settings/apps/new?state="),
    "{}",
    done.form
  );
  let manifest = served_manifest(&done.form)?;
  assert_eq!(manifest.get("name"), Some(&json!("toolu-runner-acme")));
  assert_eq!(
    manifest.get("default_permissions"),
    Some(&json!({"organization_self_hosted_runners": "write"}))
  );
  for line in [
    "App created \u{b7} toolu-runner-acme \u{b7} App 424242",
    "(0600)",
    "Install the App on acme:",
    "https://github.com/apps/toolu-runner-acme/installations/new/permissions?target_id=7",
    "Installed \u{b7} installation 1 \u{b7} all repositories",
  ] {
    assert!(
      done.stdout.contains(line),
      "missing {line:?} in:\n{}",
      done.stdout
    );
  }
  let app = saved(home.path())?;
  assert_eq!(app.get("installation_id"), Some(&json!(1)));
  assert_eq!(app.get("owner_id"), Some(&json!(7)));
  assert_eq!(app.get("owner"), Some(&json!("acme")));
  assert_no_secrets(&format!("{}{}", done.stdout, done.stderr), &server).await?;
  Ok(())
}

/// AC-7: without `--org` the personal form and `administration: write` are
/// unchanged; no org check is made; the wait ends on the owner's
/// installation.
#[tokio::test]
async fn personal_create_keeps_the_personal_form_and_permission() -> TestResult {
  let home = tempfile::tempdir()?;
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path(format!("/app-manifests/{CODE}/conversions")))
    .respond_with(conversion("octocat", 9)?)
    .mount(&server)
    .await;
  mount_installations(&server, home.path(), installed_on("octocat", "all")?).await;

  let args = ["--account", "octocat"];
  let done = run(create_app(home.path(), &server, &args, 60_000), true).await?;
  assert_eq!(
    done.code,
    Some(0),
    "stdout:\n{}\nstderr:\n{}",
    done.stdout,
    done.stderr
  );
  assert!(
    done
      .form
      .contains("action=\"https://github.com/settings/apps/new?state=")
  );
  let manifest = served_manifest(&done.form)?;
  assert_eq!(manifest.get("name"), Some(&json!("toolu-runner-octocat")));
  assert_eq!(
    manifest.get("default_permissions"),
    Some(&json!({"administration": "write"}))
  );
  let requests = server.received_requests().await.ok_or("recording off")?;
  assert!(requests.iter().all(|r| !r.url.path().starts_with("/orgs/")));
  assert_eq!(saved(home.path())?.get("installation_id"), Some(&json!(1)));
  Ok(())
}

/// AC-3 / S2 unattended: no installation ever appears; with no terminal the
/// wait gives up with exit 1, naming the link and the resume command, and
/// the App stays saved without an installation.
#[tokio::test]
async fn unattended_wait_times_out_with_exit_1() -> TestResult {
  let home = tempfile::tempdir()?;
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path(format!("/app-manifests/{CODE}/conversions")))
    .respond_with(conversion("octocat", 9)?)
    .mount(&server)
    .await;
  mount_installations(
    &server,
    home.path(),
    ResponseTemplate::new(200).set_body_json(json!([])),
  )
  .await;

  let done = run(create_app(home.path(), &server, &[], 300), true).await?;
  assert_eq!(
    done.code,
    Some(1),
    "stdout:\n{}\nstderr:\n{}",
    done.stdout,
    done.stderr
  );
  assert!(
    done.stderr.contains("isn't installed on octocat"),
    "{}",
    done.stderr
  );
  assert!(
    done.stderr.contains(
      "https://github.com/apps/toolu-runner-octocat/installations/new/permissions?target_id=9"
    ),
    "{}",
    done.stderr
  );
  assert!(
    done.stderr.contains("create-app --import"),
    "{}",
    done.stderr
  );
  let app = saved(home.path())?;
  assert_eq!(app.get("app_id"), Some(&json!(APP_ID)));
  assert!(app.get("installation_id").is_none(), "{app}");
  Ok(())
}

/// AC-5: a mistyped org (the live 404 capture) exits 2 before any browser
/// URL is printed or any file written.
#[tokio::test]
async fn unknown_org_exits_2_before_any_browser() -> TestResult {
  let home = tempfile::tempdir()?;
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/orgs/acmee"))
    .respond_with(capture("orgs_404.json")?)
    .expect(1)
    .mount(&server)
    .await;
  let done = run(
    create_app(home.path(), &server, &["--org", "acmee"], 60_000),
    false,
  )
  .await?;
  assert_eq!(done.code, Some(2), "{}", done.stderr);
  assert!(
    done
      .stderr
      .contains("No organization \"acmee\" on github.com. Check the spelling."),
    "{}",
    done.stderr
  );
  assert!(!done.stdout.contains("127.0.0.1"), "{}", done.stdout);
  assert!(!home.path().join("github-app.json").exists());
  Ok(())
}

/// `GET /app` for the test App, owned by `acme` (id 7).
async fn mount_app(server: &MockServer) -> TestResult {
  let reply = edited_reply("app_200.json", |app| {
    set_json(app, "/id", json!(APP_ID))?;
    set_json(app, "/slug", json!("toolu-runner-acme"))?;
    set_json(
      app,
      "/html_url",
      json!("https://github.com/apps/toolu-runner-acme"),
    )?;
    set_json(app, "/owner/login", json!("acme"))?;
    set_json(app, "/owner/id", json!(7))
  })?;
  Mock::given(method("GET"))
    .and(path("/app"))
    .respond_with(AppJwtGate(reply))
    .mount(server)
    .await;
  Ok(())
}

/// AC-6: `--import` proves the key with `GET /app`, refreshes the identity
/// from it, finds the installation on the owner and saves it.
#[tokio::test]
async fn import_with_a_matching_key_records_the_installation() -> TestResult {
  let (source, home) = (tempfile::tempdir()?, tempfile::tempdir()?);
  let file = write_app(source.path(), "github.com")?;
  let server = MockServer::start().await;
  mount_app(&server).await?;
  Mock::given(method("GET"))
    .and(path("/app/installations"))
    .respond_with(AppJwtGate(installed_on("acme", "all")?))
    .mount(&server)
    .await;
  let file_arg = file.to_string_lossy().into_owned();
  let done = run(
    create_app(home.path(), &server, &["--import", &file_arg], 60_000),
    false,
  )
  .await?;
  assert_eq!(
    done.code,
    Some(0),
    "stdout:\n{}\nstderr:\n{}",
    done.stdout,
    done.stderr
  );
  assert!(
    done
      .stdout
      .contains("Key matches App 424242 \u{b7} installed on acme"),
    "{}",
    done.stdout
  );
  let app = saved(home.path())?;
  assert_eq!(app.get("installation_id"), Some(&json!(1)));
  assert_eq!(app.get("slug"), Some(&json!("toolu-runner-acme")));
  assert_eq!(app.get("owner_id"), Some(&json!(7)));
  Ok(())
}

/// AC-6 / S3: a key the App does not hold (`--app-id 999` with the test key)
/// gets GitHub's live 401 — exit 2, nothing saved; with `--force` over an
/// existing App, that App is left byte-identical.
#[tokio::test]
async fn import_with_a_mismatched_key_saves_nothing() -> TestResult {
  let home = tempfile::tempdir()?;
  let key = home.path().join("key.pem");
  std::fs::write(&key, PRIVATE_PEM)?;
  let key_arg = key.to_string_lossy().into_owned();
  let server = MockServer::start().await;
  mount_app(&server).await?;
  let args = ["--app-id", "999", "--app-key", &key_arg];

  let done = run(create_app(home.path(), &server, &args, 60_000), false).await?;
  assert_eq!(done.code, Some(2), "{}", done.stderr);
  assert!(
    done.stderr.contains("does not match App 999"),
    "{}",
    done.stderr
  );
  assert!(
    done
      .stderr
      .contains("A JSON web token could not be decoded"),
    "{}",
    done.stderr
  );
  assert!(!home.path().join("github-app.json").exists());

  let existing = b"{\"pre-existing\": true}".to_vec();
  std::fs::write(home.path().join("github-app.json"), &existing)?;
  let forced = ["--app-id", "999", "--app-key", &key_arg, "--force"];
  let done = run(create_app(home.path(), &server, &forced, 60_000), false).await?;
  assert_eq!(done.code, Some(2), "{}", done.stderr);
  assert_eq!(
    std::fs::read(home.path().join("github-app.json"))?,
    existing
  );
  Ok(())
}

/// No line of the test key, not the client secret, and none of the exact
/// bearer credentials (App JWTs, installation tokens) the binary sent to
/// `server` appear in `text`.
async fn assert_no_secrets(text: &str, server: &MockServer) -> TestResult {
  for line in PRIVATE_PEM.lines().filter(|line| line.len() > 20) {
    assert!(!text.contains(line), "a PEM line leaked: {text}");
  }
  for secret in ["toolu-test-client-secret", "PLACEHOLDER_CLIENT_SECRET"] {
    assert!(!text.contains(secret), "a client secret leaked: {text}");
  }
  let requests = server.received_requests().await.ok_or("recording off")?;
  let bearers: Vec<&str> = requests
    .iter()
    .filter_map(|r| r.headers.get("authorization")?.to_str().ok())
    .filter_map(|value| value.strip_prefix("Bearer "))
    .collect();
  assert!(!bearers.is_empty(), "the binary sent no bearer credential");
  for bearer in bearers {
    assert!(!text.contains(bearer), "a bearer credential leaked: {text}");
  }
  Ok(())
}

/// AC-6: `--app-id` + `--app-key` with the App's own key: the identity comes
/// from `GET /app` (it starts empty), no client secret is invented, and the
/// installation is recorded.
#[tokio::test]
async fn app_id_and_key_with_a_matching_key_saves_the_app_from_github() -> TestResult {
  let home = tempfile::tempdir()?;
  let key = home.path().join("key.pem");
  std::fs::write(&key, PRIVATE_PEM)?;
  let key_arg = key.to_string_lossy().into_owned();
  let server = MockServer::start().await;
  mount_app(&server).await?;
  Mock::given(method("GET"))
    .and(path("/app/installations"))
    .respond_with(AppJwtGate(installed_on("acme", "all")?))
    .mount(&server)
    .await;
  let args = ["--app-id", "424242", "--app-key", &key_arg];
  let done = run(create_app(home.path(), &server, &args, 60_000), false).await?;
  assert_eq!(
    done.code,
    Some(0),
    "stdout:\n{}\nstderr:\n{}",
    done.stdout,
    done.stderr
  );
  assert!(
    done
      .stdout
      .contains("Key matches App 424242 \u{b7} installed on acme")
  );
  let app = saved(home.path())?;
  assert_eq!(app.get("slug"), Some(&json!("toolu-runner-acme")));
  assert_eq!(app.get("owner"), Some(&json!("acme")));
  assert_eq!(app.get("owner_id"), Some(&json!(7)));
  assert_eq!(
    app.get("html_url"),
    Some(&json!("https://github.com/apps/toolu-runner-acme"))
  );
  assert_eq!(app.get("client_id"), Some(&json!("Iv1.ab1112223334445c")));
  assert_eq!(app.get("client_secret"), Some(&json!("")));
  assert_eq!(app.get("installation_id"), Some(&json!(1)));
  assert_no_secrets(&format!("{}{}", done.stdout, done.stderr), &server).await?;
  Ok(())
}

/// AC-6 + S2: `--import --org acme` of an App not yet installed saves it,
/// waits through an empty poll and records the installation on acme.
#[tokio::test]
async fn import_with_org_waits_for_the_org_installation() -> TestResult {
  let (source, home) = (tempfile::tempdir()?, tempfile::tempdir()?);
  let file = write_app(source.path(), "github.com")?;
  let server = MockServer::start().await;
  mount_app(&server).await?;
  Mock::given(method("GET"))
    .and(path("/app/installations"))
    .respond_with(AppJwtGate(
      ResponseTemplate::new(200).set_body_json(json!([])),
    ))
    .up_to_n_times(2)
    .mount(&server)
    .await;
  Mock::given(method("GET"))
    .and(path("/app/installations"))
    .respond_with(AppJwtGate(installed_on("acme", "all")?))
    .mount(&server)
    .await;
  let file_arg = file.to_string_lossy().into_owned();
  let args = ["--import", &file_arg, "--org", "acme"];
  let done = run(create_app(home.path(), &server, &args, 60_000), false).await?;
  assert_eq!(
    done.code,
    Some(0),
    "stdout:\n{}\nstderr:\n{}",
    done.stdout,
    done.stderr
  );
  assert!(
    done.stdout.contains("Key matches App 424242\n"),
    "{}",
    done.stdout
  );
  assert!(
    done.stdout.contains("Install the App on acme:"),
    "{}",
    done.stdout
  );
  assert!(
    done
      .stdout
      .contains("Installed \u{b7} installation 1 \u{b7} all repositories"),
    "{}",
    done.stdout
  );
  assert_eq!(saved(home.path())?.get("installation_id"), Some(&json!(1)));
  Ok(())
}

/// The import guard: another saved App is not replaced without `--force`
/// (checked before any request); re-importing the saved file onto itself
/// needs no `--force`.
#[tokio::test]
async fn import_keeps_another_saved_app_and_may_reimport_itself() -> TestResult {
  let (source, home) = (tempfile::tempdir()?, tempfile::tempdir()?);
  let other = write_app(source.path(), "github.com")?;
  let saved_file = write_app(home.path(), "github.com")?;
  let before = std::fs::read(&saved_file)?;
  let server = MockServer::start().await;
  mount_app(&server).await?;
  Mock::given(method("GET"))
    .and(path("/app/installations"))
    .respond_with(AppJwtGate(installed_on("acme", "all")?))
    .mount(&server)
    .await;

  let other_arg = other.to_string_lossy().into_owned();
  let done = run(
    create_app(home.path(), &server, &["--import", &other_arg], 60_000),
    false,
  )
  .await?;
  assert_eq!(done.code, Some(2), "{}", done.stderr);
  assert!(
    done.stderr.contains("pass --force to overwrite it"),
    "{}",
    done.stderr
  );
  assert_eq!(std::fs::read(&saved_file)?, before);
  assert!(
    server
      .received_requests()
      .await
      .ok_or("recording off")?
      .is_empty()
  );

  let self_arg = saved_file.to_string_lossy().into_owned();
  let done = run(
    create_app(home.path(), &server, &["--import", &self_arg], 60_000),
    false,
  )
  .await?;
  assert_eq!(
    done.code,
    Some(0),
    "stdout:\n{}\nstderr:\n{}",
    done.stdout,
    done.stderr
  );
  assert_eq!(saved(home.path())?.get("installation_id"), Some(&json!(1)));
  Ok(())
}

/// Ctrl-C during the wait exits 130, keeps the App saved and names the
/// resume command (with the `--org` the wait targeted).
#[tokio::test]
async fn ctrl_c_during_the_wait_exits_130_with_the_resume_command() -> TestResult {
  let (source, home) = (tempfile::tempdir()?, tempfile::tempdir()?);
  let file = write_app(source.path(), "github.com")?;
  let server = MockServer::start().await;
  mount_app(&server).await?;
  Mock::given(method("GET"))
    .and(path("/app/installations"))
    .respond_with(AppJwtGate(
      ResponseTemplate::new(200).set_body_json(json!([])),
    ))
    .mount(&server)
    .await;
  let file_arg = file.to_string_lossy().into_owned();
  let args = ["--import", &file_arg, "--org", "acme"];
  let mut child = create_app(home.path(), &server, &args, 60_000).spawn()?;
  let pid = child.id().ok_or("no pid")?;
  let mut stdout = BufReader::new(child.stdout.take().ok_or("no stdout")?);
  loop {
    let mut line = String::new();
    if stdout.read_line(&mut line).await? == 0 {
      return Err("the wait never started".into());
    }
    if line.starts_with("Waiting for the installation") {
      break;
    }
  }
  let killed = std::process::Command::new("kill")
    .args(["-INT", &pid.to_string()])
    .status()?;
  assert!(killed.success());
  let output = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output()).await??;
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert_eq!(output.status.code(), Some(130), "{stderr}");
  assert!(
    stderr.contains("cancelled \u{2014} the App is saved at"),
    "{stderr}"
  );
  assert!(stderr.contains("create-app --import"), "{stderr}");
  assert!(stderr.contains("--org acme`"), "{stderr}");
  assert!(home.path().join("github-app.json").exists());
  Ok(())
}

/// A fatal reply during the wait that echoes the App JWT is printed masked,
/// with the saved App and the resume command.
#[tokio::test]
async fn a_fatal_wait_error_is_masked_and_names_the_resume_command() -> TestResult {
  let (source, home) = (tempfile::tempdir()?, tempfile::tempdir()?);
  let file = write_app(source.path(), "github.com")?;
  let server = MockServer::start().await;
  mount_app(&server).await?;
  Mock::given(method("GET"))
    .and(path("/app/installations"))
    .respond_with(AppJwtGate(
      ResponseTemplate::new(200).set_body_json(json!([])),
    ))
    .up_to_n_times(1)
    .mount(&server)
    .await;
  Mock::given(method("GET"))
    .and(path("/app/installations"))
    .respond_with(EchoAuthorization(401))
    .mount(&server)
    .await;
  let file_arg = file.to_string_lossy().into_owned();
  let args = ["--import", &file_arg, "--org", "acme"];
  let done = run(create_app(home.path(), &server, &args, 60_000), false).await?;
  assert_eq!(done.code, Some(2), "{}", done.stderr);
  assert!(
    done.stderr.contains("upstream echoed: Bearer ***"),
    "{}",
    done.stderr
  );
  assert!(
    done.stderr.contains("the App is saved at"),
    "{}",
    done.stderr
  );
  assert!(done.stderr.contains("--org acme`"), "{}", done.stderr);
  assert_no_secrets(&format!("{}{}", done.stdout, done.stderr), &server).await?;
  Ok(())
}

/// A `--org` that is not a GitHub login never reaches a URL.
#[tokio::test]
async fn an_org_that_is_not_a_login_exits_2_before_any_request() -> TestResult {
  let home = tempfile::tempdir()?;
  let server = MockServer::start().await;
  let done = run(
    create_app(home.path(), &server, &["--org", "a/../b"], 60_000),
    false,
  )
  .await?;
  assert_eq!(done.code, Some(2), "{}", done.stderr);
  assert!(
    done
      .stderr
      .contains("--org 'a/../b' is not a GitHub account name"),
    "{}",
    done.stderr
  );
  assert!(
    server
      .received_requests()
      .await
      .ok_or("recording off")?
      .is_empty()
  );
  Ok(())
}
