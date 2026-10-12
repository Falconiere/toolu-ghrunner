//! Tests for the `check` environment report: URL redaction, credential
//! extraction for the masker, effective proxy selection and the CGI guard.

use super::{ProxyReport, ProxyVar, TrustReport, redact_url, url_secrets};

fn var(name: &'static str, value: &str) -> ProxyVar {
  ProxyVar {
    name,
    value: value.to_owned(),
  }
}

#[test]
fn redact_url_hides_userinfo_and_drops_path_and_query() {
  assert_eq!(
    redact_url("http://alice:s3cr3t-pw@proxy.internal:3128/path?token=abc"),
    "http://***@proxy.internal:3128"
  );
  assert_eq!(
    redact_url("http://alice@proxy.internal:3128"),
    "http://***@proxy.internal:3128"
  );
}

#[test]
fn redact_url_keeps_a_credential_free_url_and_socket_paths() {
  assert_eq!(
    redact_url("http://proxy.internal:3128/"),
    "http://proxy.internal:3128"
  );
  assert_eq!(
    redact_url("unix:///var/run/docker.sock"),
    "unix:///var/run/docker.sock"
  );
  assert_eq!(
    redact_url("tcp://bob:hunter22@10.0.0.5:2376"),
    "tcp://***@10.0.0.5:2376"
  );
}

#[test]
fn redact_url_treats_a_schemeless_proxy_as_http_without_leaking() {
  // hyper-util defaults a scheme-less proxy value to http; the `url` crate
  // would otherwise parse `alice` as the scheme and print the password.
  assert_eq!(
    redact_url("alice:s3cr3t-pw@proxy.internal:3128"),
    "http://***@proxy.internal:3128"
  );
}

#[test]
fn redact_url_never_echoes_an_unparseable_or_empty_value() {
  let shown = redact_url("http://[not a host:s3cr3t-pw");
  assert!(!shown.contains("s3cr3t-pw"), "{shown}");
  assert_eq!(shown, "<set, unparseable — value hidden>");
  assert_eq!(redact_url(""), "(set, empty)");
}

#[test]
fn url_secrets_lists_every_credential_spelling() {
  let secrets = url_secrets("http://alice:s3cr3t-pw@proxy.internal:3128");
  for expected in ["alice", "s3cr3t-pw", "alice:s3cr3t-pw"] {
    assert!(
      secrets.iter().any(|s| s == expected),
      "{expected} missing: {secrets:?}"
    );
  }
  assert!(url_secrets("http://proxy.internal:3128").is_empty());
  // An unparseable value is masked whole: nothing about it is trusted.
  assert_eq!(
    url_secrets("http://[bad:s3cr3t-pw"),
    vec!["http://[bad:s3cr3t-pw"]
  );
}

#[test]
fn url_secrets_percent_decodes_like_the_proxy_auth_header() {
  let secrets = url_secrets("http://alice:p%40ss%3Aword@proxy.internal:3128");
  assert!(secrets.iter().any(|s| s == "p@ss:word"), "{secrets:?}");
  assert!(secrets.iter().any(|s| s == "p%40ss%3Aword"), "{secrets:?}");
}

#[test]
fn effective_proxy_prefers_the_scheme_variable_then_all_proxy() {
  let report = ProxyReport {
    https: None,
    http: Some(var("HTTP_PROXY", "http://h.internal:1")),
    all: Some(var("all_proxy", "http://a.internal:2")),
    no: None,
    cgi: false,
  };
  assert_eq!(report.effective(true).map(|v| v.name), Some("all_proxy"));
  assert_eq!(report.effective(false).map(|v| v.name), Some("HTTP_PROXY"));
}

#[test]
fn unparseable_scheme_variable_falls_back_to_all_proxy_like_hyper_util() {
  let report = ProxyReport {
    https: Some(var("HTTPS_PROXY", "")),
    http: None,
    all: Some(var("ALL_PROXY", "http://a.internal:2")),
    no: None,
    cgi: false,
  };
  assert_eq!(report.effective(true).map(|v| v.name), Some("ALL_PROXY"));
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
  let lines = report.lines().join("\n");
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
  let lines = report.lines().join("\n");
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
