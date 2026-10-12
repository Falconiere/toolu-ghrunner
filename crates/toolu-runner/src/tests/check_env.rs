//! Tests for the `check` environment report's rendering. Proxy selection,
//! redaction and credential extraction are `network::proxy_env`'s, tested in
//! `crates/network/tests/proxy_env_test.rs`.

use network::ProxyVar;

use super::{ProxyReport, TrustReport, proxy_lines};

fn var(name: &'static str, value: &str) -> ProxyVar {
  ProxyVar {
    name,
    value: value.to_owned(),
  }
}

#[test]
fn cgi_mode_disables_every_proxy_and_says_so() {
  let report = ProxyReport {
    https: Some(var("HTTPS_PROXY", "http://h.internal:1")),
    http: Some(var("HTTP_PROXY", "http://h.internal:1")),
    all: None,
    no: None,
    cgi: true,
  };
  assert!(report.effective(true).is_none());
  assert!(report.effective(false).is_none());
  let lines = proxy_lines(&report).join("\n");
  assert!(lines.contains("REQUEST_METHOD is set"), "{lines}");
}

#[test]
fn proxy_lines_redact_and_report_unset_variables() {
  let report = ProxyReport {
    https: Some(var(
      "https_proxy",
      "http://alice:s3cr3t-pw@proxy.internal:3128",
    )),
    http: None,
    all: None,
    no: Some(var("NO_PROXY", "localhost,.internal")),
    cgi: false,
  };
  let lines = proxy_lines(&report).join("\n");
  assert!(!lines.contains("s3cr3t-pw"), "{lines}");
  assert!(
    lines.contains("https_proxy  http://***@proxy.internal:3128"),
    "{lines}"
  );
  assert!(lines.contains("HTTP_PROXY   (unset)"), "{lines}");
  assert!(
    lines.contains("NO_PROXY     localhost,.internal"),
    "{lines}"
  );
  assert!(
    lines.contains("https:// via https_proxy; http:// direct"),
    "{lines}"
  );
}

#[test]
fn trust_lines_name_the_override_and_the_anchor_count() {
  let report = TrustReport {
    cert_file: Some("/etc/custom/ca.pem".to_owned()),
    cert_dir: None,
    node_extra_ca_certs: Some("/etc/custom/node.pem".to_owned()),
    anchors: 1,
    errors: vec![],
  };
  let lines = report.lines().join("\n");
  assert!(
    lines.contains("SSL_CERT_FILE        /etc/custom/ca.pem (replaces the platform store)"),
    "{lines}"
  );
  assert!(
    lines.contains("anchors              1 loaded, 0 errors"),
    "{lines}"
  );
  assert!(
    lines.contains("NODE_EXTRA_CA_CERTS  /etc/custom/node.pem"),
    "{lines}"
  );
  assert!(
    lines.contains(
      "bundled              Mozilla root set compiled in (webpki-roots), always trusted too"
    ),
    "{lines}"
  );
  assert!(!report.is_degraded());
}

#[test]
fn zero_anchors_is_degraded_and_says_only_bundled_roots_remain() {
  let report = TrustReport {
    cert_file: Some("/nonexistent.pem".to_owned()),
    cert_dir: None,
    node_extra_ca_certs: None,
    anchors: 0,
    errors: vec!["could not load PEM file \"/nonexistent.pem\"".to_owned()],
  };
  let lines = report.lines().join("\n");
  assert!(report.is_degraded());
  assert!(lines.contains("0 loaded, 1 errors"), "{lines}");
  assert!(
    lines.contains("only the bundled roots remain; a private CA will fail"),
    "{lines}"
  );
  assert!(lines.contains("could not load PEM file"), "{lines}");
}
