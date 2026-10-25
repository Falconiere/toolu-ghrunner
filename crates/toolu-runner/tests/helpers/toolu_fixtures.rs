//! Shared fixtures for the #206 toolu.sh poll-loop tests.
//!
//! One wiremock peer is both toolu.sh and GitHub. Its `/runner/*` routes are
//! a small state machine over the contract's bodies
//! (`fixtures/toolu_sh/`): `GET /runner/jobs/queued` lists the jobs nobody
//! claimed yet, a claim answers a fresh real-keypair JIT config
//! (`boot_fixtures::real_jit_config_b64`) pointing back at this peer, whose
//! broker routes (`boot_fixtures`) serve one real shell step per lifecycle.
//! Faults are injected per test ([`Faults`]); the 401s replay the live
//! `dev-console.toolu.sh` capture. Every `/runner/*` request is logged with
//! its arrival time, bearer, body and the status it got.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use crate::boot_fixtures;
use crate::supervisor_fixtures::TestResult;

/// The synthetic host token every fixture machine holds (`tlu_h_` + 43
/// base64url characters, the contract's shape; not a real credential).
pub const HOST_TOKEN: &str = "tlu_h_pollagent206pollagent206pollagent206pollag";

/// The fixture machine's toolu.sh org (its registration dir name).
pub const ORG: &str = "org_test";

/// The live-captured 401 (`fixtures/toolu_sh/invalid_token_401.json`).
const INVALID_TOKEN_401: &str = include_str!("../fixtures/toolu_sh/invalid_token_401.json");

/// Faults the peer injects.
#[derive(Default, Clone)]
pub struct Faults {
  /// The first `n` job lists answer 503.
  pub queued_503: usize,
  /// Once this many `complete`s arrived, every job list answers the live 401.
  pub revoke_after_completes: Option<usize>,
  /// Claims of these job ids answer `409 claim refused: taken`.
  pub taken: Vec<String>,
  /// Every heartbeat answers the live 401.
  pub heartbeat_401: bool,
  /// Every heartbeat answers `409` (the claim was lost).
  pub heartbeat_409: bool,
  /// The first claims answer these `(status, error)` refusals, in order
  /// (whatever job they name; the job stays queued).
  pub claim_refusals: Vec<(u16, &'static str)>,
  /// Every `POST` of this verb (`claim`, `dispatched`, `complete`) answers
  /// the live 401.
  pub revoke_verb: Option<&'static str>,
  /// The first `n` completes answer 503.
  pub complete_503: usize,
  /// Claim deadlines are now + this many ms (default 600 000; negative =
  /// already passed).
  pub deadline_ms: Option<i64>,
  /// #207 pairing: what each `POST /runner/pair/token` of the n-th pairing
  /// answers, in order (`authorization_pending`, `slow_down`,
  /// `access_denied`, `expired_token`, `approved`, or `503` for a transient
  /// failure); the last entry
  /// repeats, and a poll after the grant answers `expired_token`.
  pub pair_scripts: Vec<Vec<&'static str>>,
  /// #207: the `interval` a pairing asks for, in seconds (default 1).
  pub pair_interval: Option<u64>,
  /// #207: single-use enrollment tokens `POST /runner/enroll` accepts.
  pub enroll_tokens: Vec<String>,
  /// #207: how `/runner/host` misbehaves, if at all.
  pub host_fault: Option<HostFault>,
}

/// A `/runner/host` fault (#207).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HostFault {
  /// Every call answers the live 401 (the machine was revoked).
  Revoked,
  /// `DELETE` answers 503.
  Delete503,
}

/// One `/runner/*` request as the peer saw it.
#[derive(Clone, Debug)]
pub struct Seen {
  /// When it arrived.
  pub at: Instant,
  /// `GET` / `POST`.
  pub method: String,
  /// The URL path.
  pub path: String,
  /// The `Authorization` header.
  pub authorization: Option<String>,
  /// The JSON body (`Null` when none).
  pub body: Value,
  /// The status the peer answered.
  pub status: u16,
}

/// The toolu.sh side's state.
struct State {
  uri: String,
  queued: Vec<String>,
  lease_ms: u64,
  faults: Faults,
  lists: usize,
  claims: usize,
  completes: usize,
  failed_completes: usize,
  jits: Vec<String>,
  seen: Vec<Seen>,
  /// The live 401 capture's body.
  revoked_body: String,
  /// #207: pairings started so far.
  pairings: usize,
  /// #207: token polls of the current pairing.
  pair_polls: usize,
  /// #207: whether the current pairing already delivered its token.
  delivered: bool,
  /// #207: machines toolu.sh created (grants delivered).
  machines: usize,
  /// #207: the machine's stored labels.
  host_labels: Vec<String>,
}

/// A running peer and a handle on its toolu.sh state.
pub struct Peer {
  /// The wiremock server (toolu.sh + GitHub broker).
  pub server: MockServer,
  state: Arc<Mutex<State>>,
}

impl Peer {
  /// Start a peer queueing `jobs`, granting `lease_ms` leases, with `faults`;
  /// each claimed JIT runs one shell step `script`.
  pub async fn start(
    jobs: &[&str],
    lease_ms: u64,
    faults: Faults,
    script: &str,
  ) -> TestResult<Self> {
    let server = MockServer::start().await;
    boot_fixtures::mount_auth_and_session(&server).await;
    boot_fixtures::mount_job_lifecycle(&server, script).await?;
    Mock::given(method("DELETE"))
      .and(path_regex("^/session"))
      .respond_with(ResponseTemplate::new(200))
      .mount(&server)
      .await;
    let state = Arc::new(Mutex::new(State {
      uri: server.uri(),
      queued: jobs.iter().map(|j| (*j).to_owned()).collect(),
      lease_ms,
      faults,
      lists: 0,
      claims: 0,
      completes: 0,
      failed_completes: 0,
      jits: Vec::new(),
      seen: Vec::new(),
      revoked_body: invalid_token_body()?,
      pairings: 0,
      pair_polls: 0,
      delivered: false,
      machines: 0,
      host_labels: Vec::new(),
    }));
    let shared = Arc::clone(&state);
    Mock::given(path_regex("^/runner/"))
      .respond_with(move |request: &Request| respond(&shared, request))
      .mount(&server)
      .await;
    Ok(Self { server, state })
  }

  /// The peer's state. `into_inner` recovers the guard and never panics;
  /// every lock holder is panic-free by the crate's lints, and a poisoned
  /// state would already answer every request 500 (see [`respond`]).
  fn state(&self) -> MutexGuard<'_, State> {
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Every `/runner/*` request so far, in arrival order.
  pub fn seen(&self) -> Vec<Seen> {
    self.state().seen.clone()
  }

  /// Every JIT config the peer handed out.
  pub fn jits(&self) -> Vec<String> {
    self.state().jits.clone()
  }

  /// `/runner/*` requests whose path ends with `suffix`.
  pub fn seen_ending(&self, suffix: &str) -> Vec<Seen> {
    self
      .seen()
      .into_iter()
      .filter(|s| s.path.ends_with(suffix))
      .collect()
  }

  /// Wait until `suffix` requests number at least `min`.
  pub async fn wait_for(&self, suffix: &str, min: usize) -> TestResult<()> {
    let deadline = Instant::now() + crate::supervisor_fixtures::BUDGET;
    while self.seen_ending(suffix).len() < min {
      if Instant::now() > deadline {
        return Err(format!("never saw {min} x {suffix}; saw {:#?}", self.paths()).into());
      }
      tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
  }

  /// Machines toolu.sh created so far (pair grants + accepted enrollments).
  pub fn machines(&self) -> usize {
    self.state().machines
  }

  /// `METHOD path status` of every `/runner/*` request (for failure messages).
  pub fn paths(&self) -> Vec<String> {
    self
      .seen()
      .iter()
      .map(|s| format!("{} {} {}", s.method, s.path, s.status))
      .collect()
  }
}

/// Epoch milliseconds now; `None` for a clock before the epoch.
fn now_ms() -> Option<u64> {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .ok()
    .and_then(|d| u64::try_from(d.as_millis()).ok())
}

/// The live 401 capture's body; an error when the capture lacks one.
fn invalid_token_body() -> TestResult<String> {
  let capture: Value = serde_json::from_str(INVALID_TOKEN_401)?;
  capture
    .pointer("/response/body")
    .and_then(Value::as_str)
    .map(str::to_owned)
    .ok_or_else(|| "fixtures/toolu_sh/invalid_token_401.json has no /response/body".into())
}

/// A 500 the test sees in [`Peer::paths`] when the fixture itself broke.
fn fixture_broken(why: &str) -> ResponseTemplate {
  ResponseTemplate::new(500).set_body_string(format!("toolu fixture broken: {why}"))
}

/// The toolu.sh state machine (see the module doc). A poisoned state (an
/// earlier responder panicked) or a clock before the epoch answers 500
/// rather than carry on with bogus state.
fn respond(state: &Mutex<State>, request: &Request) -> ResponseTemplate {
  let Ok(mut state) = state.lock() else {
    return fixture_broken("state poisoned by an earlier panic");
  };
  let Some(now) = now_ms() else {
    return fixture_broken("system clock is before the Unix epoch");
  };
  let path = request.url.path().to_owned();
  let sent = recorded_body(&request.body);
  let (status, body) = route(&mut state, request.method.as_str(), &path, now, &sent);
  state.seen.push(Seen {
    at: Instant::now(),
    method: request.method.to_string(),
    path,
    authorization: request
      .headers
      .get("authorization")
      .and_then(|v| v.to_str().ok())
      .map(str::to_owned),
    body: sent,
    status,
  });
  ResponseTemplate::new(status)
    .insert_header("content-type", "application/json")
    .set_body_string(body)
}

/// A request body as recorded: `Null` when empty (a GET), else its JSON, or
/// its raw text when it does not parse, so a body assertion shows what came.
fn recorded_body(bytes: &[u8]) -> Value {
  if bytes.is_empty() {
    return Value::Null;
  }
  serde_json::from_slice(bytes)
    .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(bytes).into_owned()))
}

/// The status and body for one request.
fn route(
  state: &mut State,
  method_name: &str,
  path: &str,
  now: u64,
  sent: &Value,
) -> (u16, String) {
  let ok = || json!({ "ok": true }).to_string();
  match (method_name, path.rsplit('/').next().unwrap_or_default()) {
    ("POST", "pair") => pair(state, sent),
    ("POST", "token") => pair_token(state, sent),
    ("POST", "enroll") => enroll(state, sent),
    (_, "host") if state.faults.host_fault == Some(HostFault::Revoked) => {
      (401, state.revoked_body.clone())
    },
    ("GET", "host") => (200, host_body(state)),
    ("PATCH", "host") => {
      if let Some(labels) = sent.get("labels").and_then(Value::as_array) {
        state.host_labels = labels
          .iter()
          .filter_map(Value::as_str)
          .map(str::to_owned)
          .collect();
      }
      (200, host_body(state))
    },
    ("DELETE", "host") if state.faults.host_fault == Some(HostFault::Delete503) => {
      (503, json!({ "error": "unavailable" }).to_string())
    },
    ("DELETE", "host") => (204, String::new()),
    ("GET", "queued") => list(state),
    ("POST", verb) if state.faults.revoke_verb == Some(verb) => (401, state.revoked_body.clone()),
    ("POST", "claim") => claim(state, &job_id(path), now),
    ("POST", "heartbeat") if state.faults.heartbeat_401 => (401, state.revoked_body.clone()),
    ("POST", "heartbeat") if state.faults.heartbeat_409 => {
      (409, json!({ "error": "claim superseded" }).to_string())
    },
    ("POST", "heartbeat") => (
      200,
      json!({ "leaseExpiresAt": now + state.lease_ms }).to_string(),
    ),
    ("POST", "dispatched") => (200, ok()),
    ("POST", "complete") if state.failed_completes < state.faults.complete_503 => {
      state.failed_completes += 1;
      (503, json!({ "error": "unavailable" }).to_string())
    },
    ("POST", "complete") => {
      state.completes += 1;
      (200, ok())
    },
    _ => (404, json!({ "error": "not found" }).to_string()),
  }
}

/// `GET /runner/jobs/queued`: the unclaimed jobs, after the injected 503s;
/// the live 401 once revoked.
fn list(state: &mut State) -> (u16, String) {
  state.lists += 1;
  if state.lists <= state.faults.queued_503 {
    return (503, json!({ "error": "unavailable" }).to_string());
  }
  if state
    .faults
    .revoke_after_completes
    .is_some_and(|n| state.completes >= n)
  {
    return (401, state.revoked_body.clone());
  }
  let jobs: Vec<Value> = state
    .queued
    .iter()
    .map(|id| json!({ "id": id, "runnerTag": "toolu-self-hosted", "labels": [], "createdAt": 1 }))
    .collect();
  (200, json!({ "jobs": jobs }).to_string())
}

/// `POST /runner/jobs/:id/claim`: the injected refusals first, a lost race
/// for a `taken` job, else a fresh real JIT config and a `lease_ms` lease.
fn claim(state: &mut State, id: &str, now: u64) -> (u16, String) {
  state.claims += 1;
  if let Some((status, error)) = state.faults.claim_refusals.get(state.claims - 1) {
    return (*status, json!({ "error": error }).to_string());
  }
  if state.faults.taken.iter().any(|t| t == id) {
    state.queued.retain(|j| j != id);
    return (409, json!({ "error": "claim refused: taken" }).to_string());
  }
  let Some(pos) = state.queued.iter().position(|j| j == id) else {
    return (
      404,
      json!({ "error": "claim refused: not-found" }).to_string(),
    );
  };
  state.queued.remove(pos);
  let Ok(jit) = boot_fixtures::real_jit_config_b64(&state.uri) else {
    return (
      503,
      json!({ "error": "claim refused: mint-failed" }).to_string(),
    );
  };
  state.jits.push(jit.clone());
  let deadline = now.saturating_add_signed(state.faults.deadline_ms.unwrap_or(600_000));
  let body = json!({
    "jitConfig": jit,
    "deadline": deadline,
    "claimEpoch": 7,
    "leaseExpiresAt": now + state.lease_ms,
  });
  (200, body.to_string())
}

/// The fixture machine's host id.
pub const HOST_ID: &str = "83dca27b-4052-420a-a767-04c5cd758654";

/// The fixture org's display name (`GET /runner/host` `org.name`).
pub const WORKSPACE: &str = "Example team";

/// `POST /runner/pair`: a fresh pairing with the configured interval.
fn pair(state: &mut State, sent: &Value) -> (u16, String) {
  state.pairings += 1;
  state.pair_polls = 0;
  state.delivered = false;
  state.host_labels = labels_of(sent);
  let code = format!("PAIR-{:04}", state.pairings);
  let body = json!({
    "device_code": format!("devicecode207-{}", state.pairings),
    "user_code": code,
    "verification_uri": format!("{}/pair", state.uri),
    "verification_uri_complete": format!("{}/pair?code={code}", state.uri),
    "expires_in": 900,
    "interval": state.faults.pair_interval.unwrap_or(1),
  });
  (200, body.to_string())
}

/// `POST /runner/pair/token`: the current pairing's script, step by step.
fn pair_token(state: &mut State, sent: &Value) -> (u16, String) {
  let expected = format!("devicecode207-{}", state.pairings);
  if state.delivered || sent.get("device_code").and_then(Value::as_str) != Some(&expected) {
    return (400, json!({ "error": "expired_token" }).to_string());
  }
  let script = state
    .faults
    .pair_scripts
    .get(state.pairings.saturating_sub(1))
    .or_else(|| state.faults.pair_scripts.last());
  let step = script
    .and_then(|s| s.get(state.pair_polls).or_else(|| s.last()))
    .copied()
    .unwrap_or("approved");
  state.pair_polls += 1;
  if step == "503" {
    return (503, json!({ "error": "unavailable" }).to_string());
  }
  if step != "approved" {
    return (400, json!({ "error": step }).to_string());
  }
  state.delivered = true;
  (200, grant(state))
}

/// `POST /runner/enroll`: a known, unused token creates a machine once.
fn enroll(state: &mut State, sent: &Value) -> (u16, String) {
  let token = sent
    .get("enroll_token")
    .and_then(Value::as_str)
    .unwrap_or_default();
  let Some(pos) = state.faults.enroll_tokens.iter().position(|t| t == token) else {
    return (401, json!({ "error": "invalid_enroll_token" }).to_string());
  };
  state.faults.enroll_tokens.remove(pos);
  state.host_labels = labels_of(sent);
  (200, grant(state))
}

/// A grant for a new machine (counted).
fn grant(state: &mut State) -> String {
  state.machines += 1;
  json!({
    "host_token": HOST_TOKEN,
    "host_id": HOST_ID,
    "org": ORG,
    "labels": state.host_labels,
  })
  .to_string()
}

/// The request's `labels`.
fn labels_of(sent: &Value) -> Vec<String> {
  sent
    .get("labels")
    .and_then(Value::as_array)
    .map(|labels| {
      labels
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
    })
    .unwrap_or_default()
}

/// `GET` / `PATCH /runner/host` 200 (an authenticated call enables it).
fn host_body(state: &State) -> String {
  json!({
    "host_id": HOST_ID,
    "name": "Build box",
    "hostname": "worker-7",
    "org": { "id": ORG, "name": WORKSPACE },
    "platform": "linux",
    "arch": "x64",
    "labels": state.host_labels,
    "slots": 1,
    "runner_version": env!("CARGO_PKG_VERSION"),
    "enabled": true,
    "last_seen_at": now_ms(),
  })
  .to_string()
}

/// The `:id` of `/runner/jobs/:id/<verb>`.
fn job_id(path: &str) -> String {
  path
    .strip_prefix("/runner/jobs/")
    .and_then(|rest| rest.split('/').next())
    .unwrap_or_default()
    .to_owned()
}

/// Write `<home>/toolu/<ORG>/{config.toml,credentials.json}` as #207 will:
/// `[provider] kind = "toolu"` at `api_url`, no GitHub fields, the host token
/// as `access_token`. `extra` is appended to the top-level keys.
pub fn write_machine(home: &Path, api_url: &str, token: &str, extra: &str) -> TestResult<PathBuf> {
  let dir = home.join("toolu").join(ORG);
  std::fs::create_dir_all(dir.join("_diag"))?;
  let config_path = dir.join("config.toml");
  std::fs::write(&config_path, machine_toml(home, api_url, extra))?;
  std::fs::write(
    dir.join("credentials.json"),
    json!({ "access_token": token, "issued_at": "2026-10-24T00:00:00Z" }).to_string(),
  )?;
  Ok(config_path)
}

/// The machine's `config.toml` text.
pub fn machine_toml(home: &Path, api_url: &str, extra: &str) -> String {
  let dir = home.join("toolu").join(ORG);
  format!(
    "labels = []\n{extra}\n[provider]\nkind = \"toolu\"\napi_url = \"{api_url}\"\nhost_id = \
     \"83dca27b-4052-420a-a767-04c5cd758654\"\norg = \"{ORG}\"\n\n[runtime]\nwork_dir = \
     \"{}\"\ndata_dir = \"{}\"\n",
    home.join("_work").display(),
    dir.display()
  )
}
