//! `check` subcommand: a read-only preflight equivalent to upstream
//! `run.sh --check`. Probes the GitHub service endpoints the runner needs
//! (concurrently, each bounded), reports git / Docker / Node availability
//! and the effective proxy and TLS-trust settings, then exits with an
//! aggregate code. Never takes the job lock, writes a file, opens a session
//! or registers anything.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ::config::config::load_config;
use clap::Args;
use shared::SecretMasker;
use tokio::task::JoinSet;
use url::Url;

use crate::check_endpoints::{
  Endpoint, JitEndpoints, Probe, endpoints_for, jit_endpoints, parse_target_url,
};
use crate::check_env::{ProxyReport, TrustReport, url_secrets};
use crate::check_probe::{FailureKind, ProbeOutcome, build_client, probe_http};
use crate::check_report::{EndpointResult, Report, render};
use crate::check_tools::{probe_docker, probe_git, probe_node};
use crate::config;

/// Exit code for a usage or configuration error before any probe runs.
const EXIT_USAGE: i32 = 2;

/// Extended help footer for `check --help`.
const CHECK_AFTER_HELP: &str = "\
Probes (required unless noted): api, codeload, pipelines, broker, token,
results; node-dist (optional). Any HTTP answer counts as reachable — no
credentials are sent — except 407 Proxy Authentication Required. git, docker
and node are optional and only ever warn.

Examples:
  toolu-runner check                                   # the resolved registration, else github.com
  toolu-runner check --url https://ghes.example.com    # a GHES instance, before registering
  toolu-runner check --timeout 30                      # slow links

Exit codes:
  0  every applicable required endpoint answered (optional warnings allowed)
  1  at least one required endpoint failed
  2  usage or configuration error before probing";

/// `check` arguments.
#[derive(Debug, Args)]
#[command(after_help = CHECK_AFTER_HELP)]
pub(crate) struct CheckArgs {
  /// GitHub URL whose hosts to probe (github.com, GHES, or a repo/org URL on
  /// either). Overrides any registration.
  #[arg(long, value_name = "URL", conflicts_with = "config")]
  pub(crate) url: Option<String>,
  /// Registration whose JIT endpoints to probe. Default: resolved like
  /// `run` (cwd-inferred > the sole registration); none found → github.com.
  #[arg(long, value_name = "PATH", value_hint = clap::ValueHint::FilePath)]
  pub(crate) config: Option<PathBuf>,
  /// Per-probe bound in seconds (1–300).
  #[arg(long, value_name = "SECS", default_value_t = 10,
        value_parser = clap::value_parser!(u64).range(1..=300))]
  pub(crate) timeout: u64,
}

/// The target hosts and where they came from.
struct Target {
  base: Url,
  jit: Option<JitEndpoints>,
  data_dir: Option<PathBuf>,
  origin: String,
}

/// Load a registration's runner URL, JIT endpoints and data dir.
fn target_from_registration(path: &Path) -> Result<Target, String> {
  let cfg = load_config(path).map_err(|e| format!("cannot load {}: {e}", path.display()))?;
  Ok(Target {
    base: parse_target_url(&cfg.runner_url)?,
    jit: Some(jit_endpoints(&cfg.runtime.jit_config)?),
    data_dir: Some(shared::paths::expand_tilde(Path::new(
      &cfg.runtime.data_dir,
    ))),
    origin: format!("registration: {}", path.display()),
  })
}

/// `--url` > `--config` > the registration `run` would use > github.com.
fn resolve_target(args: &CheckArgs) -> Result<Target, String> {
  if let Some(raw) = &args.url {
    return Ok(Target {
      base: parse_target_url(raw)?,
      jit: None,
      data_dir: None,
      origin: "--url".to_owned(),
    });
  }
  if let Some(path) = &args.config {
    return target_from_registration(path);
  }
  match crate::resolve_config(None) {
    Ok(path) => target_from_registration(&path),
    Err(why) => Ok(Target {
      base: parse_target_url("https://github.com")?,
      jit: None,
      data_dir: None,
      origin: format!("defaults: {why}"),
    }),
  }
}

/// Probe every endpoint concurrently; skipped ones carry no outcome.
async fn probe_endpoints(endpoints: Vec<Endpoint>, timeout: Duration) -> Vec<EndpointResult> {
  let client = build_client(timeout);
  let mut set = JoinSet::new();
  for (index, endpoint) in endpoints.iter().enumerate() {
    let Probe::Get(url) = endpoint.probe.clone() else {
      continue;
    };
    let client = client.clone();
    set.spawn(async move {
      let outcome = match client {
        Ok(client) => probe_http(&client, &url).await,
        Err(detail) => ProbeOutcome::Failed {
          kind: FailureKind::Tls,
          detail,
        },
      };
      (index, outcome)
    });
  }
  let mut outcomes = vec![None; endpoints.len()];
  while let Some(joined) = set.join_next().await {
    match joined {
      Ok((index, outcome)) => {
        if let Some(slot) = outcomes.get_mut(index) {
          *slot = Some(outcome);
        }
      },
      Err(error) => eprintln!("toolu-runner: check probe task failed: {error}"),
    }
  }
  endpoints
    .into_iter()
    .zip(outcomes)
    .map(|(endpoint, outcome)| EndpointResult { endpoint, outcome })
    .collect()
}

/// A masker seeded with every credential the printed settings could echo.
fn masker_for(proxy: &ProxyReport) -> SecretMasker {
  let mut masker = SecretMasker::new();
  let docker = config::docker_host()
    .map(|host| url_secrets(&host))
    .unwrap_or_default();
  for secret in proxy.secrets().into_iter().chain(docker) {
    masker.add_secret(&secret);
  }
  masker
}

/// Run `check`; returns the process exit code.
pub(crate) async fn cmd_check(args: CheckArgs) -> i32 {
  let target = match resolve_target(&args) {
    Ok(target) => target,
    Err(error) => {
      eprintln!("toolu-runner: {error}");
      return EXIT_USAGE;
    },
  };
  let timeout = Duration::from_secs(args.timeout);
  let proxy = ProxyReport::from_env();
  let masker = masker_for(&proxy);
  let endpoints = endpoints_for(&target.base, target.jit.as_ref());
  let trust = tokio::task::spawn_blocking(TrustReport::from_env);
  let (endpoints, git, docker, node) = tokio::join!(
    probe_endpoints(endpoints, timeout),
    probe_git(timeout),
    probe_docker(timeout),
    probe_node(timeout, target.data_dir.as_deref()),
  );
  let trust = trust.await.unwrap_or_else(|error| TrustReport {
    cert_file: config::ssl_cert_file(),
    cert_dir: config::ssl_cert_dir(),
    node_extra_ca_certs: config::node_extra_ca_certs(),
    anchors: 0,
    errors: vec![format!("trust store load panicked: {error}")],
  });
  let report = Report {
    target: format!(
      "{} ({})",
      target.base.host_str().unwrap_or_default(),
      target.origin
    ),
    endpoints,
    tools: vec![git, docker, node],
    proxy,
    trust,
  };
  print!("{}", render(&report, &masker));
  report.exit_code()
}
