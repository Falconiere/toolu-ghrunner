//! Tests for `check` report rendering, masking and exit-code aggregation.

use shared::SecretMasker;

use super::{EndpointResult, Report, render};
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
