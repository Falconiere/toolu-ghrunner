//! [`client_builder`]: proxy, bypass list and extra trust anchors.

use std::path::Path;

use reqwest::{Certificate, ClientBuilder, NoProxy, Proxy, Url};
use rustls::RootCertStore;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject as _;
use shared::{NetworkConfig, RunnerError};

use crate::proxy_env::{ProxyEnv, ProxyVar, redact_url};

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
/// `ca_bundle` that is relative, inaccessible, unreadable, not a file, or holds no valid
/// PEM certificate — never a silent fallback to default trust.
pub fn client_builder(cfg: &NetworkConfig) -> Result<ClientBuilder, RunnerError> {
  let mut builder = reqwest::Client::builder();
  if let Some(raw) = configured_proxy(cfg) {
    builder = builder.proxy(build_proxy(raw, cfg)?);
  }
  if let Some(path) = &cfg.ca_bundle {
    for der in load_bundle(path)? {
      let cert = Certificate::from_der(der.as_ref())
        .map_err(|e| RunnerError::Config(format!("[network] ca_bundle {}: {e}", path.display())))?;
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
/// Validates as it describes: a configured proxy must parse, so a caller
/// that only wants the diagnostic (`check`) still fails on an unusable value.
/// Env mode reports what reqwest actually uses ([`ProxyEnv::effective`]):
/// per scheme, `REQUEST_METHOD` and set-but-empty variables included.
///
/// # Errors
///
/// The same proxy and `ca_bundle` errors as [`client_builder`].
pub fn describe(cfg: &NetworkConfig) -> Result<String, RunnerError> {
  let proxy = match configured_proxy(cfg) {
    Some(raw) => {
      parse_proxy(raw)?;
      format!(
        "proxy {} (config), no_proxy {} entries",
        redact_url(raw),
        configured_bypass(cfg).count()
      )
    },
    None => describe_env(&ProxyEnv::from_env()),
  };
  let bundle = match &cfg.ca_bundle {
    Some(path) => format!(
      ", ca_bundle {} ({} certificates)",
      path.display(),
      load_bundle(path)?.len()
    ),
    None => String::new(),
  };
  Ok(format!("{proxy}{bundle}"))
}

/// The env-mode half of [`describe`]: the proxy each scheme goes through.
fn describe_env(env: &ProxyEnv) -> String {
  if env.cgi {
    return "proxy none (env: REQUEST_METHOD is set, proxy variables ignored)".to_owned();
  }
  let shown = |var: &ProxyVar| format!("{} (env {})", redact_url(&var.value), var.name);
  match (env.effective(true), env.effective(false)) {
    (None, None) => "proxy none".to_owned(),
    (Some(https), Some(http)) if https == http => format!("proxy {}", shown(https)),
    (https, http) => format!(
      "proxy https {}, http {}",
      https.map_or_else(|| "direct".to_owned(), shown),
      http.map_or_else(|| "direct".to_owned(), shown)
    ),
  }
}

/// Whether `[network] proxy` is set to a non-blank value (config mode).
pub fn proxy_configured(cfg: &NetworkConfig) -> bool {
  configured_proxy(cfg).is_some()
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
  /// Percent-decoded password, when present.
  pub(crate) password: Option<String>,
}

/// Parse `raw` as an `http://` / `https://` proxy URL with a host.
pub(crate) fn parse_proxy(raw: &str) -> Result<ParsedProxy, RunnerError> {
  let shown = redact_url(raw);
  let mut url = Url::parse(raw)
    .map_err(|e| RunnerError::Config(format!("[network] proxy {shown}: not a valid URL ({e})")))?;
  if !matches!(url.scheme(), "http" | "https") {
    return Err(RunnerError::Config(format!(
      "[network] proxy {shown}: scheme must be http or https"
    )));
  }
  if url.host_str().is_none_or(str::is_empty) {
    return Err(RunnerError::Config(format!(
      "[network] proxy {shown}: has no host"
    )));
  }
  let has_userinfo = !url.username().is_empty() || url.password().is_some();
  let at_outside_userinfo = url.path().contains('@')
    || url.query().is_some_and(|query| query.contains('@'))
    || url
      .fragment()
      .is_some_and(|fragment| fragment.contains('@'));
  if at_outside_userinfo || (raw.contains('@') && !has_userinfo) {
    // An unencoded `/ ? #` in a password ends the authority early:
    // `http://u:123#x@proxy` parses as host `u`, `http://u:p@ss/x@proxy` as
    // password `p` on host `ss` — either way the real credentials and host
    // are lost, and the raw value would be exported to steps unmasked.
    return Err(RunnerError::Config(format!(
      "[network] proxy {shown}: percent-encode reserved characters (@ : / ? #) in the user name and password"
    )));
  }
  let decode = |value: &str| {
    percent_encoding::percent_decode_str(value)
      .decode_utf8_lossy()
      .into_owned()
  };
  let user = Some(url.username()).filter(|u| !u.is_empty()).map(decode);
  let password = url.password().map(decode);
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
    password,
  })
}

fn build_proxy(raw: &str, cfg: &NetworkConfig) -> Result<Proxy, RunnerError> {
  let parsed = parse_proxy(raw)?;
  let shown = redact_url(raw);
  let mut proxy = Proxy::all(parsed.url.as_str())
    .map_err(|e| RunnerError::Config(format!("[network] proxy {shown}: {e}")))?;
  // The user may be empty (`http://:pw@proxy`) when a password is present.
  if parsed.user.is_some() || parsed.password.is_some() {
    proxy = proxy.basic_auth(
      parsed.user.as_deref().unwrap_or_default(),
      parsed.password.as_deref().unwrap_or_default(),
    );
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
  let meta = std::fs::metadata(path).map_err(|e| fail(format!("cannot access: {e}")))?;
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
    // A borrowed view: `certs` is returned, so no copy is needed here.
    store
      .add(CertificateDer::from(cert.as_ref()))
      .map_err(|e| {
        fail(format!(
          "certificate #{} is not a usable CA certificate: {e}",
          index + 1
        ))
      })?;
  }
  Ok(certs)
}
