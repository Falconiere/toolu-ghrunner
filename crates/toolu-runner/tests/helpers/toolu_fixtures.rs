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
    }));
    let shared = Arc::clone(&state);
    Mock::given(path_regex("^/runner/"))
      .respond_with(move |request: &Request| respond(&shared, request))
      .mount(&server)
      .await;
    Ok(Self { server, state })
  }

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

  /// `METHOD path status` of every `/runner/*` request (for failure messages).
  pub fn paths(&self) -> Vec<String> {
    self
      .seen()
      .iter()
      .map(|s| format!("{} {} {}", s.method, s.path, s.status))
      .collect()
  }
}

/// Epoch milliseconds now.
fn now_ms() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .ok()
    .and_then(|d| u64::try_from(d.as_millis()).ok())
    .unwrap_or(0)
}

/// The live 401 capture's body.
fn invalid_token_body() -> String {
  serde_json::from_str::<Value>(INVALID_TOKEN_401)
    .ok()
    .and_then(|v| {
      v.pointer("/response/body")
        .and_then(Value::as_str)
        .map(str::to_owned)
    })
    .unwrap_or_default()
}

/// The toolu.sh state machine (see the module doc).
fn respond(state: &Mutex<State>, request: &Request) -> ResponseTemplate {
  let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
  let path = request.url.path().to_owned();
  let (status, body) = route(&mut state, request.method.as_str(), &path);
  state.seen.push(Seen {
    at: Instant::now(),
    method: request.method.to_string(),
    path,
    authorization: request
      .headers
      .get("authorization")
      .and_then(|v| v.to_str().ok())
      .map(str::to_owned),
    body: serde_json::from_slice(&request.body).unwrap_or(Value::Null),
    status,
  });
  ResponseTemplate::new(status)
    .insert_header("content-type", "application/json")
    .set_body_string(body)
}

/// The status and body for one request.
fn route(state: &mut State, method_name: &str, path: &str) -> (u16, String) {
  let ok = || json!({ "ok": true }).to_string();
  match (method_name, path.rsplit('/').next().unwrap_or_default()) {
    ("GET", "queued") => list(state),
    ("POST", verb) if state.faults.revoke_verb == Some(verb) => (401, invalid_token_body()),
    ("POST", "claim") => claim(state, &job_id(path)),
    ("POST", "heartbeat") if state.faults.heartbeat_401 => (401, invalid_token_body()),
    ("POST", "heartbeat") if state.faults.heartbeat_409 => {
      (409, json!({ "error": "claim superseded" }).to_string())
    },
    ("POST", "heartbeat") => (
      200,
      json!({ "leaseExpiresAt": now_ms() + state.lease_ms }).to_string(),
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
    return (401, invalid_token_body());
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
fn claim(state: &mut State, id: &str) -> (u16, String) {
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
  let now = now_ms();
  let deadline = now.saturating_add_signed(state.faults.deadline_ms.unwrap_or(600_000));
  let body = json!({
    "jitConfig": jit,
    "deadline": deadline,
    "claimEpoch": 7,
    "leaseExpiresAt": now + state.lease_ms,
  });
  (200, body.to_string())
}

/// The `:id` of `/runner/jobs/:id/<verb>`.
fn job_id(path: &str) -> String {
  path
    .trim_start_matches("/runner/jobs/")
    .split('/')
    .next()
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
