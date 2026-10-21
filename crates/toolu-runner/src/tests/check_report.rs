//! Tests for `check` report rendering, masking and exit-code aggregation.

use shared::SecretMasker;

use super::{EndpointResult, Report, render, section_lines};
use crate::check_endpoints::{Endpoint, Probe};
use crate::check_env::{ProxyReport, TrustReport};
use crate::check_probe::{FailureKind, ProbeOutcome};
use crate::check_tools::{ToolReport, ToolStatus};

fn endpoint(name: &'static str, required: bool, outcome: Option<ProbeOutcome>) -> EndpointResult {
  let probe = if outcome.is_some() {
    Probe::Get(format!("https://{name}.example.com/"))
  } else {
    Probe::Skip("not applicable on GHES (no Results service)")
  };
  EndpointResult {
    endpoint: Endpoint {
      name,
      required,
      probe,
    },
    outcome,
  }
}

fn reachable(status: u16) -> ProbeOutcome {
  ProbeOutcome::Reachable { status, millis: 12 }
}

fn failed(kind: FailureKind, detail: &str) -> ProbeOutcome {
  ProbeOutcome::Failed {
    kind,
    detail: detail.to_owned(),
  }
}

fn report(endpoints: Vec<EndpointResult>) -> Report {
  Report {
    target: "ghes.example.com (--url)".to_owned(),
    endpoints,
    tools: vec![ToolReport {
      name: "git",
      status: ToolStatus::Warn,
      summary: "not found on PATH (…)".to_owned(),
    }],
    proxy: ProxyReport {
      https: None,
      http: None,
      all: None,
      no: None,
      cgi: false,
    },
    network: None,
    proxy_configured: false,
    trust: TrustReport {
      cert_file: None,
      cert_dir: None,
      node_extra_ca_certs: None,
      anchors: 140,
      errors: vec![],
    },
  }
}

#[test]
fn all_required_reachable_with_optional_warnings_exits_zero() {
  let report = report(vec![
    endpoint("api", true, Some(reachable(200))),
    endpoint("token", true, Some(reachable(401))),
    endpoint("results", true, None),
    endpoint(
      "node-dist",
      false,
      Some(failed(FailureKind::Timeout, "operation timed out")),
    ),
  ]);
  assert_eq!(report.exit_code(), 0);
  let text = render(&report, &SecretMasker::new());
  assert!(
    text.contains("  PASS  api        https://api.example.com/  HTTP 200  12 ms"),
    "{text}"
  );
  assert!(
    text.contains("  PASS  token      https://token.example.com/  HTTP 401"),
    "{text}"
  );
  assert!(
    text.contains("  SKIP  results    not applicable on GHES"),
    "{text}"
  );
  assert!(
    text.contains("  WARN  node-dist  https://node-dist.example.com/  timeout:"),
    "{text}"
  );
  assert!(
    text.contains("  WARN  git        not found on PATH"),
    "{text}"
  );
  assert!(
    text.contains("result: all required checks passed (exit 0)"),
    "{text}"
  );
}

#[test]
fn a_required_failure_exits_one_and_prints_the_hint() {
  let report = report(vec![
    endpoint(
      "api",
      true,
      Some(failed(
        FailureKind::Dns,
        "dns error: failed to lookup address",
      )),
    ),
    endpoint(
      "broker",
      true,
      Some(failed(FailureKind::Connect, "connection refused")),
    ),
  ]);
  assert_eq!(report.required_failures(), 2);
  assert_eq!(report.exit_code(), 1);
  let text = render(&report, &SecretMasker::new());
  assert!(
    text.contains("  FAIL  api        https://api.example.com/  dns: dns error"),
    "{text}"
  );
  assert!(text.contains("hint: the name did not resolve"), "{text}");
  assert!(
    text.contains("result: 2 required checks failed (exit 1)"),
    "{text}"
  );
}

#[test]
fn every_line_passes_through_the_masker() {
  let report = report(vec![endpoint(
    "api",
    true,
    Some(failed(
      FailureKind::Proxy,
      "tunnel to proxy alice:s3cr3t-pw failed",
    )),
  )]);
  let mut masker = SecretMasker::new();
  masker.add_secret("s3cr3t-pw");
  let text = render(&report, &masker);
  assert!(!text.contains("s3cr3t-pw"), "{text}");
  assert!(
    text.contains("proxy: tunnel to proxy alice:*** failed"),
    "{text}"
  );
  assert!(
    text.contains("result: 1 required check failed (exit 1)"),
    "{text}"
  );
}

#[test]
fn a_required_probe_without_an_outcome_fails_instead_of_passing() {
  let mut never_ran = endpoint("api", true, Some(reachable(200)));
  never_ran.outcome = None;
  let report = report(vec![never_ran]);
  assert_eq!(report.required_failures(), 1);
  assert_eq!(report.exit_code(), 1);
  let text = render(&report, &SecretMasker::new());
  assert!(
    text.contains("  FAIL  api        https://api.example.com/  error: the probe did not complete"),
    "{text}"
  );
}

#[test]
fn proxy_override_note_follows_the_typed_flag_not_the_text() {
  let mut config_mode = report(vec![endpoint("api", true, Some(reachable(200)))]);
  config_mode.network = Some("proxy http://p:1 (env)".to_owned());
  config_mode.proxy_configured = true;
  let text = render(&config_mode, &SecretMasker::new());
  assert!(
    text.contains("[network] proxy is set: the proxy variables below are ignored"),
    "{text}"
  );
  let mut env_mode = report(vec![endpoint("api", true, Some(reachable(200)))]);
  env_mode.network = Some("proxy http://p:1 (config), no_proxy 0 entries".to_owned());
  env_mode.proxy_configured = false;
  let text = render(&env_mode, &SecretMasker::new());
  assert!(
    !text.contains("the proxy variables below are ignored"),
    "{text}"
  );
}

#[test]
fn warnings_count_failed_optional_probes_and_warned_tools_only() {
  let mut optional_never_ran = endpoint("telemetry", false, Some(reachable(200)));
  optional_never_ran.outcome = None;
  let mut mixed = report(vec![
    // A required failure is a failure, not a warning.
    endpoint(
      "api",
      true,
      Some(failed(
        FailureKind::Dns,
        "dns error: failed to lookup address",
      )),
    ),
    endpoint("token", true, Some(reachable(401))),
    endpoint("results", true, None),
    // Optional probes: failed (+1), never completed (+1), reachable, skipped.
    endpoint(
      "node-dist",
      false,
      Some(failed(FailureKind::Timeout, "operation timed out")),
    ),
    optional_never_ran,
    endpoint("codeload", false, Some(reachable(200))),
    endpoint("alpine", false, None),
  ]);
  mixed.tools = vec![
    ToolReport {
      name: "git",
      status: ToolStatus::Warn,
      summary: "not found on PATH (…)".to_owned(),
    },
    ToolReport {
      name: "docker",
      status: ToolStatus::Ok,
      summary: "Docker version 27.0.0".to_owned(),
    },
    ToolReport {
      name: "node",
      status: ToolStatus::Warn,
      summary: "`node --version` failed: boom".to_owned(),
    },
  ];
  assert_eq!(mixed.warnings(), 4);
  assert_eq!(mixed.required_failures(), 1);

  let mut clean = report(vec![
    endpoint("api", true, Some(reachable(200))),
    endpoint("node-dist", false, Some(reachable(200))),
  ]);
  clean.tools = Vec::new();
  assert_eq!(clean.warnings(), 0);
  assert_eq!(clean.required_failures(), 0);
}

#[test]
fn a_required_failure_alone_is_no_warning() {
  let report = report(vec![endpoint(
    "api",
    true,
    Some(failed(FailureKind::Connect, "connection refused")),
  )]);
  // Only the `git` tool line of the helper report warns.
  assert_eq!(report.warnings(), 1);
  assert_eq!(report.required_failures(), 1);
}

#[test]
fn section_lines_omit_the_header_and_result_and_leave_secrets_for_the_caller() {
  let report = report(vec![
    endpoint("api", true, Some(reachable(200))),
    endpoint("results", true, None),
    endpoint(
      "node-dist",
      false,
      Some(failed(
        FailureKind::Proxy,
        "tunnel to proxy alice:s3cr3t-pw failed",
      )),
    ),
  ]);
  let lines = section_lines(&report);
  assert_eq!(
    lines,
    vec![
      "network (required):",
      "  PASS  api        https://api.example.com/  HTTP 200  12 ms",
      "  SKIP  results    not applicable on GHES (no Results service)",
      "network (optional):",
      "  WARN  node-dist  https://node-dist.example.com/  proxy: tunnel to proxy alice:s3cr3t-pw failed\n        hint: the proxy refused the tunnel or wants credentials: check HTTPS_PROXY / HTTP_PROXY",
      "tools (optional):",
      "  WARN  git        not found on PATH (…)",
      "proxy:",
      "  HTTPS_PROXY  (unset)",
      "  HTTP_PROXY   (unset)",
      "  ALL_PROXY    (unset)",
      "  NO_PROXY     (unset)",
      "  routing      https:// direct; http:// direct",
      "tls trust:",
      "  SSL_CERT_FILE        (unset)",
      "  SSL_CERT_DIR         (unset)",
      "  NODE_EXTRA_CA_CERTS  (unset)",
      "  anchors              140 loaded from the platform store, 0 errors",
      "  bundled              Mozilla root set compiled in (webpki-roots), always trusted too",
    ]
  );
}
