//! `check` HTTP probes: one bounded, unauthenticated GET per endpoint
//! through a client built like the runner's own (system proxy +
//! `rustls-tls-native-roots`), with failures classified into an actionable
//! kind.

use std::error::Error as _;
use std::time::{Duration, Instant};

use reqwest::StatusCode;

/// Why a probe failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureKind {
  /// No complete response within the bound.
  Timeout,
  /// The proxy refused the tunnel or asked for credentials.
  Proxy,
  /// The host name did not resolve.
  Dns,
  /// The TLS handshake or certificate verification failed.
  Tls,
  /// TCP connect refused / reset / unreachable.
  Connect,
  /// Anything else reqwest reported.
  Other,
}

impl FailureKind {
  /// Short class printed before the detail.
  pub(crate) fn label(self) -> &'static str {
    match self {
      Self::Timeout => "timeout",
      Self::Proxy => "proxy",
      Self::Dns => "dns",
      Self::Tls => "tls",
      Self::Connect => "connect",
      Self::Other => "error",
    }
  }

  /// One-line operator hint.
  pub(crate) fn hint(self) -> &'static str {
    match self {
      Self::Timeout => {
        "no response within the bound: a firewall dropping packets, or a hanging proxy"
      },
      Self::Proxy => {
        "the proxy refused the tunnel or wants credentials: check HTTPS_PROXY / HTTP_PROXY"
      },
      Self::Dns => "the name did not resolve: check DNS, the host spelling, or set HTTPS_PROXY",
      Self::Tls => {
        "TLS failed: an intercepting proxy or private CA? see `tls trust` (SSL_CERT_FILE)"
      },
      Self::Connect => "connection refused or unreachable: check firewall rules and the proxy",
      Self::Other => "see the error detail",
    }
  }
}

/// The result of one endpoint probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProbeOutcome {
  /// The target answered (any status but 407).
  Reachable {
    /// The HTTP status code.
    status: u16,
    /// Round-trip time to the response head.
    millis: u128,
  },
  /// The probe failed.
  Failed {
    /// The failure class.
    kind: FailureKind,
    /// The error chain (masked again before printing).
    detail: String,
  },
}

/// The client the probes share: production's builder defaults (system
/// proxy, native roots) plus the per-probe bound and no redirects, so a
/// status is the target's own.
///
/// # Errors
///
/// The builder's error (e.g. a native trust store with only invalid
/// certificates).
pub(crate) fn build_client(timeout: Duration) -> Result<reqwest::Client, String> {
  reqwest::Client::builder()
    // api.github.com answers 403 to a request without a User-Agent.
    .user_agent(concat!("toolu-runner/", env!("CARGO_PKG_VERSION"), " (check)"))
    .timeout(timeout)
    .connect_timeout(timeout)
    .redirect(reqwest::redirect::Policy::none())
    .build()
    .map_err(|e| format!("HTTP client: {}", error_chain(&e)))
}

/// GET `url` once. Any response counts as reachable — configured broker,
/// tenant and token URLs answer 401/404 to an unauthenticated request —
/// except `407 Proxy Authentication Required`.
pub(crate) async fn probe_http(client: &reqwest::Client, url: &str) -> ProbeOutcome {
  let started = Instant::now();
  match client.get(url).send().await {
    Ok(response) if response.status() == StatusCode::PROXY_AUTHENTICATION_REQUIRED => {
      ProbeOutcome::Failed {
        kind: FailureKind::Proxy,
        detail: "HTTP 407 Proxy Authentication Required".to_owned(),
      }
    },
    Ok(response) => ProbeOutcome::Reachable {
      status: response.status().as_u16(),
      millis: started.elapsed().as_millis(),
    },
    Err(error) => ProbeOutcome::Failed {
      kind: classify(&error),
      detail: error_chain(&error),
    },
  }
}

/// Classify a reqwest error from its flags and source chain. Proxy tunnel
/// failures surface as connect errors, so the chain text is checked first.
/// Only the *sources* are matched: the top-level message embeds the URL,
/// whose host could contain any of the needles.
pub(crate) fn classify(error: &reqwest::Error) -> FailureKind {
  if error.is_timeout() {
    return FailureKind::Timeout;
  }
  let mut chain = String::new();
  let mut source = error.source();
  while let Some(cause) = source {
    chain.push_str(&cause.to_string().to_ascii_lowercase());
    chain.push('\n');
    source = cause.source();
  }
  let has = |needles: &[&str]| needles.iter().any(|needle| chain.contains(needle));
  if has(&["tunnel", "proxy"]) {
    FailureKind::Proxy
  } else if has(&[
    "dns error",
    "failed to lookup address",
    "name or service not known",
  ]) {
    FailureKind::Dns
  } else if has(&["certificate", "tls", "handshake", "corrupt message", "peer"]) {
    FailureKind::Tls
  } else if error.is_connect() {
    FailureKind::Connect
  } else {
    FailureKind::Other
  }
}

/// `error: source: source …`, de-duplicated, capped at 300 characters.
fn error_chain(error: &reqwest::Error) -> String {
  let mut parts = vec![error.to_string()];
  let mut source = error.source();
  while let Some(cause) = source {
    let text = cause.to_string();
    if !parts.iter().any(|part| part.contains(&text)) {
      parts.push(text);
    }
    source = cause.source();
  }
  let joined = parts.join(": ");
  match joined.char_indices().nth(300) {
    Some((cut, _)) => format!("{}…", joined.get(..cut).unwrap_or_default()),
    None => joined,
  }
}
