//! `check` environment report: the proxy and TLS-trust settings the runner's
//! HTTP clients actually use, printed with every credential redacted.
//!
//! Proxy selection mirrors hyper-util's `Matcher::from_system` (what every
//! reqwest client in the runner uses): per pair the first *set* variable
//! wins, a scheme variable that does not parse falls back to `ALL_PROXY`,
//! and `REQUEST_METHOD` disables all of them. Trust is the union reqwest
//! builds: rustls-native-certs' anchors (`SSL_CERT_FILE` / `SSL_CERT_DIR`
//! replace the platform store when either is set) PLUS the compiled-in
//! Mozilla set (`rustls-tls-webpki-roots`), which nothing can remove — so the
//! variables can add a private CA but never narrow trust.

use url::Url;

use crate::config;

/// Shown in place of a value that does not parse — it may hold a secret.
const UNPARSEABLE: &str = "<set, unparseable — value hidden>";

/// One proxy variable as read from the environment.
#[derive(Debug, Clone)]
pub(crate) struct ProxyVar {
  /// The variable's exact name (`HTTPS_PROXY` or `https_proxy`, …).
  pub(crate) name: &'static str,
  /// Its raw value — never printed; see [`redact_url`].
  pub(crate) value: String,
}

/// The proxy variables in effect for this process.
#[derive(Debug, Clone)]
pub(crate) struct ProxyReport {
  /// `HTTPS_PROXY` / `https_proxy`.
  pub(crate) https: Option<ProxyVar>,
  /// `HTTP_PROXY` / `http_proxy`.
  pub(crate) http: Option<ProxyVar>,
  /// `ALL_PROXY` / `all_proxy`.
  pub(crate) all: Option<ProxyVar>,
  /// `NO_PROXY` / `no_proxy`.
  pub(crate) no: Option<ProxyVar>,
  /// `REQUEST_METHOD` is set: every proxy variable is ignored.
  pub(crate) cgi: bool,
}

impl ProxyReport {
  /// Read the proxy variables through the crate's `config` module.
  pub(crate) fn from_env() -> Self {
    let to_var = |(name, value): (&'static str, String)| ProxyVar { name, value };
    Self {
      https: config::https_proxy().map(to_var),
      http: config::http_proxy().map(to_var),
      all: config::all_proxy().map(to_var),
      no: config::no_proxy().map(to_var),
      cgi: config::cgi_request(),
    }
  }

  /// The proxy an `https://` (`true`) or `http://` (`false`) destination
  /// goes through before `NO_PROXY` is applied, or `None` for direct.
  pub(crate) fn effective(&self, https: bool) -> Option<&ProxyVar> {
    if self.cgi {
      return None;
    }
    let scheme = if https { &self.https } else { &self.http };
    scheme
      .as_ref()
      .filter(|var| parses(&var.value))
      .or_else(|| self.all.as_ref().filter(|var| parses(&var.value)))
  }

  /// Every credential spelling the proxy values carry, for the masker.
  pub(crate) fn secrets(&self) -> Vec<String> {
    [&self.https, &self.http, &self.all]
      .into_iter()
      .flatten()
      .flat_map(|var| url_secrets(&var.value))
      .collect()
  }

  /// The report's `proxy:` section, one line per variable plus the
  /// per-scheme routing summary.
  pub(crate) fn lines(&self) -> Vec<String> {
    let pairs = [
      (&self.https, "HTTPS_PROXY"),
      (&self.http, "HTTP_PROXY"),
      (&self.all, "ALL_PROXY"),
    ];
    let mut lines: Vec<String> = pairs
      .iter()
      .map(|(var, default_name)| match var {
        Some(var) => format!("{:<12} {}", var.name, redact_url(&var.value)),
        None => format!("{default_name:<12} (unset)"),
      })
      .collect();
    lines.push(match &self.no {
      Some(var) => format!("{:<12} {}", var.name, var.value),
      None => format!("{:<12} (unset)", "NO_PROXY"),
    });
    if self.cgi {
      lines.push("REQUEST_METHOD is set: every proxy variable is ignored (CGI guard)".to_owned());
    }
    let route = |https: bool| {
      self
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

/// Parse a proxy or daemon URL the way hyper-util reads a proxy variable: a
/// value without `://` is taken as `http://`.
fn parse(raw: &str) -> Option<Url> {
  if raw.contains("://") {
    Url::parse(raw).ok()
  } else {
    Url::parse(&format!("http://{raw}")).ok()
  }
  .filter(|url| url.has_host() || url.path().len() > 1)
}

/// Whether a proxy value is usable (hyper-util ignores one that does not
/// parse, falling back to `ALL_PROXY`).
fn parses(raw: &str) -> bool {
  !raw.is_empty() && parse(raw).is_some_and(|url| url.has_host())
}

/// `scheme://[***@]host[:port]` for a proxy or `DOCKER_HOST` value, with
/// userinfo replaced by `***`, query and fragment dropped, and the path kept
/// only for host-less socket URLs (`unix:///var/run/docker.sock`). Never
/// echoes a value it cannot parse.
pub(crate) fn redact_url(raw: &str) -> String {
  if raw.is_empty() {
    return "(set, empty)".to_owned();
  }
  let Some(url) = parse(raw) else {
    return UNPARSEABLE.to_owned();
  };
  let Some(host) = url.host_str().filter(|host| !host.is_empty()) else {
    return format!("{}://{}", url.scheme(), url.path());
  };
  let userinfo = if url.username().is_empty() && url.password().is_none() {
    ""
  } else {
    "***@"
  };
  let port = url
    .port()
    .map(|port| format!(":{port}"))
    .unwrap_or_default();
  format!("{}://{userinfo}{host}{port}", url.scheme())
}

/// Every spelling of the credentials in a proxy or `DOCKER_HOST` value —
/// user, password, `user:password`, raw and percent-decoded — for the
/// masker. An unparseable value is returned whole.
pub(crate) fn url_secrets(raw: &str) -> Vec<String> {
  let Some(url) = parse(raw) else {
    return if raw.is_empty() {
      vec![]
    } else {
      vec![raw.to_owned()]
    };
  };
  let user = url.username();
  let password = url.password().unwrap_or_default();
  let mut secrets = Vec::new();
  for (encoded, decoded) in [user, password].map(|part| (part, percent_decode(part))) {
    secrets.push(encoded.to_owned());
    secrets.push(decoded);
  }
  if !user.is_empty() && !password.is_empty() {
    secrets.push(format!("{user}:{password}"));
    secrets.push(format!(
      "{}:{}",
      percent_decode(user),
      percent_decode(password)
    ));
  }
  secrets.retain(|secret| !secret.is_empty());
  secrets.dedup();
  secrets
}

/// Decode `%XX` escapes (the form a proxy's Basic credentials are sent in);
/// a malformed escape is kept verbatim.
fn percent_decode(raw: &str) -> String {
  let bytes = raw.as_bytes();
  let mut out = Vec::with_capacity(bytes.len());
  let mut index = 0;
  while let Some(&byte) = bytes.get(index) {
    let escaped = (byte == b'%')
      .then(|| bytes.get(index + 1..index + 3))
      .flatten()
      .and_then(|hex| std::str::from_utf8(hex).ok())
      .and_then(|hex| u8::from_str_radix(hex, 16).ok());
    if let Some(decoded) = escaped {
      out.push(decoded);
      index += 3;
    } else {
      out.push(byte);
      index += 1;
    }
  }
  String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
#[path = "tests/check_env.rs"]
mod tests;
