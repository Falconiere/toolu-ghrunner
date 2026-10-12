//! `check` environment report: the proxy and TLS-trust settings the runner's
//! HTTP clients actually use, printed with every credential redacted.
//!
//! Proxy selection, URL redaction and credential extraction are the
//! `network` crate's single implementation ([`network::ProxyEnv`],
//! [`network::redact_url`], [`network::url_secrets`]), which mirrors
//! hyper-util's `Matcher::from_system`; this module only renders them.
//! Trust is the union reqwest builds: rustls-native-certs' anchors (`SSL_CERT_FILE` / `SSL_CERT_DIR`
//! replace the platform store when either is set) PLUS the compiled-in
//! Mozilla set (`rustls-tls-webpki-roots`), which nothing can remove — so the
//! variables can add a private CA but never narrow trust.

pub(crate) use network::{redact_url, url_secrets};

use crate::config;

/// The proxy variables in effect for this process (`network`'s reading).
pub(crate) type ProxyReport = network::ProxyEnv;

/// The report's `proxy:` section, one line per variable plus the
/// per-scheme routing summary.
pub(crate) fn proxy_lines(report: &ProxyReport) -> Vec<String> {
  let pairs = [
    (&report.https, "HTTPS_PROXY"),
    (&report.http, "HTTP_PROXY"),
    (&report.all, "ALL_PROXY"),
  ];
  let mut lines: Vec<String> = pairs
    .iter()
    .map(|(var, default_name)| match var {
      Some(var) => format!("{:<12} {}", var.name, redact_url(&var.value)),
      None => format!("{default_name:<12} (unset)"),
    })
    .collect();
  lines.push(match &report.no {
    Some(var) => format!("{:<12} {}", var.name, var.value),
    None => format!("{:<12} (unset)", "NO_PROXY"),
  });
  if report.cgi {
    lines.push("REQUEST_METHOD is set: every proxy variable is ignored (CGI guard)".to_owned());
  }
  let route = |https: bool| {
    report
      .effective(https)
      .map_or_else(|| "direct".to_owned(), |var| format!("via {}", var.name))
  };
  lines.push(format!(
    "routing      https:// {}; http:// {}",
    route(true),
    route(false)
  ));
  lines
}

/// The TLS trust anchors the runner's HTTP clients load.
#[derive(Debug, Clone)]
pub(crate) struct TrustReport {
  /// `SSL_CERT_FILE`, when set.
  pub(crate) cert_file: Option<String>,
  /// `SSL_CERT_DIR`, when set.
  pub(crate) cert_dir: Option<String>,
  /// `NODE_EXTRA_CA_CERTS`, when set (seen by Node steps).
  pub(crate) node_extra_ca_certs: Option<String>,
  /// Trust anchors `rustls_native_certs::load_native_certs` returned.
  pub(crate) anchors: usize,
  /// Load errors it reported.
  pub(crate) errors: Vec<String>,
}

impl TrustReport {
  /// Load the trust anchors exactly as reqwest's `rustls-tls-native-roots`
  /// does and record the related variables.
  pub(crate) fn from_env() -> Self {
    let loaded = rustls_native_certs::load_native_certs();
    Self {
      cert_file: config::ssl_cert_file(),
      cert_dir: config::ssl_cert_dir(),
      node_extra_ca_certs: config::node_extra_ca_certs(),
      anchors: loaded.certs.len(),
      errors: loaded.errors.iter().map(ToString::to_string).collect(),
    }
  }

  /// No platform / `SSL_CERT_*` anchors loaded: only the bundled Mozilla
  /// roots remain, so a private or intercepting CA will fail.
  pub(crate) fn is_degraded(&self) -> bool {
    self.anchors == 0
  }

  /// The report's `tls trust:` section.
  pub(crate) fn lines(&self) -> Vec<String> {
    let overridden = self.cert_file.is_some() || self.cert_dir.is_some();
    let source = |value: &Option<String>| match value {
      Some(path) if overridden => format!("{path} (replaces the platform store)"),
      Some(path) => path.clone(),
      None => "(unset)".to_owned(),
    };
    let mut lines = vec![
      format!("{:<20} {}", "SSL_CERT_FILE", source(&self.cert_file)),
      format!("{:<20} {}", "SSL_CERT_DIR", source(&self.cert_dir)),
      format!(
        "{:<20} {}",
        "NODE_EXTRA_CA_CERTS",
        self.node_extra_ca_certs.as_deref().unwrap_or("(unset)")
      ),
    ];
    let store = if overridden {
      ""
    } else {
      " from the platform store"
    };
    let mut anchors = format!(
      "{:<20} {} loaded{store}, {} errors",
      "anchors",
      self.anchors,
      self.errors.len()
    );
    if self.is_degraded() {
      anchors.push_str(" — only the bundled roots remain; a private CA will fail");
    }
    lines.push(anchors);
    lines.push(format!(
      "{:<20} Mozilla root set compiled in (webpki-roots), always trusted too",
      "bundled"
    ));
    lines.extend(
      self
        .errors
        .iter()
        .map(|error| format!("{:<20} {error}", "load error")),
    );
    lines
  }
}

#[cfg(test)]
#[path = "tests/check_env.rs"]
mod tests;
