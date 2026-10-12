//! `check` report: renders every probe, tool, proxy and trust line through
//! the secret masker, and derives the aggregate exit code.
//!
//! Exit codes: `0` every applicable required endpoint answered (optional
//! warnings allowed), `1` at least one required endpoint failed. Usage and
//! configuration errors (`2`) are decided before a report exists.

use shared::SecretMasker;

use crate::check_endpoints::{Endpoint, Probe};
use crate::check_env::{ProxyReport, TrustReport};
use crate::check_probe::ProbeOutcome;
use crate::check_tools::{ToolReport, ToolStatus};

/// Exit code when a required endpoint failed.
pub(crate) const EXIT_REQUIRED_FAILED: i32 = 1;

/// One endpoint with its outcome (`None` when skipped).
#[derive(Debug, Clone)]
pub(crate) struct EndpointResult {
  /// The endpoint probed.
  pub(crate) endpoint: Endpoint,
  /// Its outcome; `None` for a [`Probe::Skip`].
  pub(crate) outcome: Option<ProbeOutcome>,
}

/// Everything `check` found.
#[derive(Debug, Clone)]
pub(crate) struct Report {
  /// Target host and where it came from.
  pub(crate) target: String,
  /// Endpoint results in report order.
  pub(crate) endpoints: Vec<EndpointResult>,
  /// Tool lines.
  pub(crate) tools: Vec<ToolReport>,
  /// Proxy variables.
  pub(crate) proxy: ProxyReport,
  /// The registration's `[network]` settings (`network::describe`), when set.
  pub(crate) network: Option<String>,
  /// Whether `[network] proxy` is set (config mode), so the proxy variables
  /// are ignored (`network::proxy_configured`).
  pub(crate) proxy_configured: bool,
  /// TLS trust.
  pub(crate) trust: TrustReport,
}

impl Report {
  /// How many required endpoints failed. A probe that should have run but
  /// produced no outcome (its task died) counts as failed, never as passed.
  pub(crate) fn required_failures(&self) -> usize {
    self
      .endpoints
      .iter()
      .filter(|result| result.endpoint.required && failed(result))
      .count()
  }

  /// The process exit code for this report.
  pub(crate) fn exit_code(&self) -> i32 {
    if self.required_failures() == 0 {
      0
    } else {
      EXIT_REQUIRED_FAILED
    }
  }
}

/// A `Get` probe that failed or never completed.
fn failed(result: &EndpointResult) -> bool {
  match (&result.endpoint.probe, &result.outcome) {
    (Probe::Get(_), Some(ProbeOutcome::Failed { .. }) | None) => true,
    (Probe::Get(_), Some(ProbeOutcome::Reachable { .. })) | (Probe::Skip(_), _) => false,
  }
}

fn endpoint_line(result: &EndpointResult) -> String {
  let name = result.endpoint.name;
  match (&result.endpoint.probe, &result.outcome) {
    (Probe::Skip(reason), _) => format!("  SKIP  {name:<10} {reason}"),
    (Probe::Get(url), Some(ProbeOutcome::Reachable { status, millis })) => {
      format!("  PASS  {name:<10} {url}  HTTP {status}  {millis} ms")
    },
    (Probe::Get(url), Some(ProbeOutcome::Failed { kind, detail })) => {
      let tag = if result.endpoint.required {
        "FAIL"
      } else {
        "WARN"
      };
      format!(
        "  {tag}  {name:<10} {url}  {}: {detail}\n        hint: {}",
        kind.label(),
        kind.hint()
      )
    },
    (Probe::Get(url), None) => {
      let tag = if result.endpoint.required {
        "FAIL"
      } else {
        "WARN"
      };
      format!("  {tag}  {name:<10} {url}  error: the probe did not complete (see stderr)")
    },
  }
}

fn tool_line(tool: &ToolReport) -> String {
  let tag = match tool.status {
    ToolStatus::Ok => "OK  ",
    ToolStatus::Warn => "WARN",
  };
  format!("  {tag}  {:<10} {}", tool.name, tool.summary)
}

fn section(out: &mut Vec<String>, title: &str, lines: impl IntoIterator<Item = String>) {
  out.push(format!("{title}:"));
  out.extend(lines);
}

/// The `[network]` line, plus a note when its proxy overrides the variables.
fn network_lines(network: Option<&str>, proxy_configured: bool) -> Vec<String> {
  let Some(line) = network else {
    return Vec::new();
  };
  let mut lines = vec![format!("{:<12} {line}", "[network]")];
  if proxy_configured {
    lines.push("[network] proxy is set: the proxy variables below are ignored".to_owned());
  }
  lines
}

/// The full report text; every line passes through `masker`.
pub(crate) fn render(report: &Report, masker: &SecretMasker) -> String {
  let mut out = vec![format!("toolu-runner check — target {}", report.target)];
  let (required, optional): (Vec<_>, Vec<_>) = report
    .endpoints
    .iter()
    .partition(|result| result.endpoint.required);
  section(
    &mut out,
    "network (required)",
    required.into_iter().map(endpoint_line),
  );
  section(
    &mut out,
    "network (optional)",
    optional.into_iter().map(endpoint_line),
  );
  section(
    &mut out,
    "tools (optional)",
    report.tools.iter().map(tool_line),
  );
  section(
    &mut out,
    "proxy",
    network_lines(report.network.as_deref(), report.proxy_configured)
      .into_iter()
      .chain(report.proxy.lines())
      .map(|line| format!("  {line}")),
  );
  section(
    &mut out,
    "tls trust",
    report
      .trust
      .lines()
      .into_iter()
      .map(|line| format!("  {line}")),
  );
  let failures = report.required_failures();
  out.push(if failures == 0 {
    "result: all required checks passed (exit 0)".to_owned()
  } else {
    format!(
      "result: {failures} required check{} failed (exit {EXIT_REQUIRED_FAILED})",
      if failures == 1 { "" } else { "s" }
    )
  });
  let mut text = out
    .iter()
    .map(|line| masker.mask(line).into_owned())
    .collect::<Vec<_>>()
    .join("\n");
  text.push('\n');
  text
}

#[cfg(test)]
#[path = "tests/check_report.rs"]
mod tests;
