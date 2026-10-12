//! [`client_builder`]: proxy, bypass list and extra trust anchors.

use std::path::Path;

use reqwest::{Certificate, ClientBuilder, NoProxy, Proxy, Url};
use rustls::RootCertStore;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject as _;
use shared::network_redact::redact_proxy_url;
use shared::{NetworkConfig, RunnerError};

/// Destinations a configured proxy never carries (upstream `RunnerWebProxy`
/// bypasses loopback). Also keeps offline/accelerated local service URLs
/// direct for steps, which receive the same list as `NO_PROXY`.
pub const LOOPBACK_BYPASS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

/// A reqwest builder carrying the `[network]` settings; callers add only their
/// own timeouts / redirect policy before `build()`.
///
/// Config mode installs one proxy for http and https destinations with the
/// [`bypass_list`]; `ClientBuilder::proxy` disables reqwest's system proxy, so
/// proxy environment variables (and env `NO_PROXY`) stop applying. Proxy
/// credentials are percent-decoded and sent as Basic auth. Env mode leaves
/// the builder's system-proxy default untouched.
///
/// # Errors
///
/// `RunnerError::Config` for an unusable proxy URL (shown redacted) or a
/// `ca_bundle` that is relative, unreadable, not a file, or holds no valid
/// PEM certificate — never a silent fallback to default trust.
pub fn client_builder(cfg: &NetworkConfig) -> Result<ClientBuilder, RunnerError> {
  let mut builder = reqwest::Client::builder();
  if let Some(raw) = configured_proxy(cfg) {
    builder = builder.proxy(build_proxy(raw, cfg)?);
  }
  if let Some(path) = &cfg.ca_bundle {
    for der in load_bundle(path)? {
      let cert = Certificate::from_der(der.as_ref()).map_err(|e| {
        RunnerError::Config(format!("[network] ca_bundle {}: {e}", path.display()))
      })?;
      builder = builder.add_root_certificate(cert);
    }
  }
  Ok(builder)
}

/// One redacted line describing the effective network settings, for the
/// runner's startup log and `check`: `proxy <redacted> (config|env)` or
/// `proxy none`, the configured `no_proxy` entry count, and the
/// `ca_bundle` path with its certificate count.
///
/// # Errors
///
/// The same `ca_bundle` errors as [`client_builder`].
pub fn describe(cfg: &NetworkConfig) -> Result<String, RunnerError> {
  let mut line = match configured_proxy(cfg) {
    Some(raw) => format!(
      "proxy {} (config), no_proxy {} entries",
      redact_proxy_url(raw),
      configured_bypass(cfg).count()
    ),
    None => match crate::config::first_set(crate::config::HTTPS_PROXY)
      .or_else(|| crate::config::first_set(crate::config::HTTP_PROXY))
    {
      Some(env) => format!("proxy {} (env)", redact_proxy_url(&env)),
      None => "proxy none".to_owned(),
    },
  };
  if let Some(path) = &cfg.ca_bundle {
    let count = load_bundle(path)?.len();
    line.push_str(&format!(", ca_bundle {} ({count} certificates)", path.display()));
  }
  Ok(line)
}

/// The configured proxy, trimmed; blank means env mode.
pub(crate) fn configured_proxy(cfg: &NetworkConfig) -> Option<&str> {
  cfg
    .proxy
    .as_deref()
    .map(str::trim)
    .filter(|raw| !raw.is_empty())
}

/// The configured `no_proxy` entries, trimmed, blanks dropped.
fn configured_bypass(cfg: &NetworkConfig) -> impl Iterator<Item = &str> {
  cfg
    .no_proxy
    .iter()
    .map(|entry| entry.trim())
    .filter(|entry| !entry.is_empty())
}

/// Config-mode bypass list: the configured entries plus [`LOOPBACK_BYPASS`].
pub(crate) fn bypass_list(cfg: &NetworkConfig) -> Vec<String> {
  let mut list: Vec<String> = configured_bypass(cfg).map(str::to_owned).collect();
  for host in LOOPBACK_BYPASS {
    if !list.iter().any(|entry| entry.eq_ignore_ascii_case(host)) {
      list.push(host.to_owned());
    }
  }
  list
}

/// A proxy URL split into its credential-free form and decoded credentials.
pub(crate) struct ParsedProxy {
  /// The URL with userinfo removed.
  pub(crate) url: Url,
  /// Percent-decoded username, when present.
  pub(crate) user: Option<String>,
  /// The password exactly as written (percent-encoded), when present.
  pub(crate) raw_password: Option<String>,
  /// Percent-decoded password, when present.
  pub(crate) password: Option<String>,
}

/// Parse `raw` as an `http://` / `https://` proxy URL with a host.
pub(crate) fn parse_proxy(raw: &str) -> Result<ParsedProxy, RunnerError> {
  let shown = redact_proxy_url(raw);
  let mut url = Url::parse(raw)
    .map_err(|e| RunnerError::Config(format!("[network] proxy {shown}: not a valid URL ({e})")))?;
  if !matches!(url.scheme(), "http" | "https") {
    return Err(RunnerError::Config(format!(
      "[network] proxy {shown}: scheme must be http or https"
    )));
  }
  if url.host_str().is_none_or(str::is_empty) {
    return Err(RunnerError::Config(format!("[network] proxy {shown}: has no host")));
  }
  let decode = |value: &str| percent_encoding::percent_decode_str(value).decode_utf8_lossy().into_owned();
  let user = Some(url.username()).filter(|u| !u.is_empty()).map(decode);
  let raw_password = url.password().map(str::to_owned);
  let password = raw_password.as_deref().map(decode);
  // Infallible for http(s) URLs with a host; a failure would keep the
  // credentials in the URL, so treat it as unusable rather than ignore it.
  if url.set_username("").is_err() || url.set_password(None).is_err() {
    return Err(RunnerError::Config(format!(
      "[network] proxy {shown}: cannot separate its credentials"
    )));
  }
  Ok(ParsedProxy {
    url,
    user,
    raw_password,
    password,
  })
}

fn build_proxy(raw: &str, cfg: &NetworkConfig) -> Result<Proxy, RunnerError> {
  let parsed = parse_proxy(raw)?;
  let shown = redact_proxy_url(raw);
  let mut proxy = Proxy::all(parsed.url.as_str())
    .map_err(|e| RunnerError::Config(format!("[network] proxy {shown}: {e}")))?;
  if let Some(user) = &parsed.user {
    proxy = proxy.basic_auth(user, parsed.password.as_deref().unwrap_or_default());
  }
  Ok(proxy.no_proxy(NoProxy::from_string(&bypass_list(cfg).join(","))))
}

/// Read and validate a `ca_bundle`: absolute, a regular file, every PEM
/// certificate well-formed and usable as a trust anchor, at least one.
fn load_bundle(path: &Path) -> Result<Vec<CertificateDer<'static>>, RunnerError> {
  let shown = path.display();
  let fail = |what: String| RunnerError::Config(format!("[network] ca_bundle {shown}: {what}"));
  if !path.is_absolute() {
    return Err(fail("must be an absolute path".to_owned()));
  }
  let meta = std::fs::metadata(path).map_err(|e| fail(format!("cannot read: {e}")))?;
  if !meta.is_file() {
    return Err(fail("is not a regular file".to_owned()));
  }
  let bytes = std::fs::read(path).map_err(|e| fail(format!("cannot read: {e}")))?;
  let certs = CertificateDer::pem_slice_iter(&bytes)
    .collect::<Result<Vec<_>, _>>()
    .map_err(|e| fail(format!("invalid PEM: {e}")))?;
  if certs.is_empty() {
    return Err(fail("contains no PEM certificates".to_owned()));
  }
  let mut store = RootCertStore::empty();
  for (index, cert) in certs.iter().enumerate() {
    store.add(cert.clone()).map_err(|e| {
      fail(format!("certificate #{} is not a usable CA certificate: {e}", index + 1))
    })?;
  }
  Ok(certs)
}
