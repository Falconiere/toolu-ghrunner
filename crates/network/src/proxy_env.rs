//! The proxy environment as reqwest uses it, and the one way the runner
//! parses, redacts and masks a proxy (or `DOCKER_HOST`) URL.
//!
//! Selection mirrors hyper-util's `Matcher::from_system`: per pair the first
//! *set* variable wins (set-but-empty disables the pair), a scheme variable
//! that does not parse falls back to `ALL_PROXY`, and `REQUEST_METHOD`
//! disables every proxy variable. Redaction and masking fail safe: a value
//! that does not parse cleanly is never echoed, and is masked whole.

use url::Url;

use crate::config;

/// Shown in place of a value that does not parse — it may hold a secret.
pub const UNPARSEABLE: &str = "<set, unparseable — value hidden>";

/// One proxy variable as read from the environment.
#[derive(Clone, PartialEq, Eq)]
pub struct ProxyVar {
  /// The variable's exact name (`HTTPS_PROXY` or `https_proxy`, …).
  pub name: &'static str,
  /// Its raw value — never printed; see [`redact_url`].
  pub value: String,
}

impl std::fmt::Debug for ProxyVar {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("ProxyVar")
      .field("name", &self.name)
      .field("value", &redact_url(&self.value))
      .finish()
  }
}

/// The proxy variables in effect for this process.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProxyEnv {
  /// `HTTPS_PROXY` / `https_proxy`.
  pub https: Option<ProxyVar>,
  /// `HTTP_PROXY` / `http_proxy`.
  pub http: Option<ProxyVar>,
  /// `ALL_PROXY` / `all_proxy`.
  pub all: Option<ProxyVar>,
  /// `NO_PROXY` / `no_proxy`.
  pub no: Option<ProxyVar>,
  /// `REQUEST_METHOD` is set: every proxy variable is ignored.
  pub cgi: bool,
}

impl ProxyEnv {
  /// Read the proxy variables (through this crate's `config` module).
  pub fn from_env() -> Self {
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
  pub fn effective(&self, https: bool) -> Option<&ProxyVar> {
    if self.cgi {
      return None;
    }
    let scheme = if https { &self.https } else { &self.http };
    scheme
      .as_ref()
      .filter(|var| usable(&var.value))
      .or_else(|| self.all.as_ref().filter(|var| usable(&var.value)))
  }

  /// Every credential spelling the proxy values carry, for the masker —
  /// regardless of the CGI guard, since steps still inherit the values.
  pub fn secrets(&self) -> Vec<String> {
    [&self.https, &self.http, &self.all]
      .into_iter()
      .flatten()
      .flat_map(|var| url_secrets(&var.value))
      .collect()
  }
}

/// Parse a proxy or daemon URL the way hyper-util reads a proxy variable: a
/// value without a scheme (`user:pw@host:3128`) is taken as `http://`. Only
/// a leading RFC 3986 scheme followed by `://` counts as a scheme, so a `://`
/// later in the value (a password, a query) never does.
pub fn parse_url(raw: &str) -> Option<Url> {
  let with_scheme = if has_scheme(raw) {
    Url::parse(raw)
  } else {
    Url::parse(&format!("http://{raw}"))
  };
  with_scheme
    .ok()
    .filter(|url| url.has_host() || url.path().len() > 1)
}

/// `scheme://[***@]host[:port]` for a proxy or `DOCKER_HOST` value, with
/// userinfo replaced by `***`, query and fragment dropped, and the path kept
/// only for host-less socket URLs (`unix:///var/run/docker.sock`). Never
/// echoes a value that does not parse cleanly (see [`url_secrets`]).
pub fn redact_url(raw: &str) -> String {
  if raw.is_empty() {
    return "(set, empty)".to_owned();
  }
  let Some(url) = parse_clean(raw) else {
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
/// user, password, `user:password`, raw and percent-decoded, and the
/// `Proxy-Authorization` Basic credential — for the masker. Fails safe: a
/// value that does not parse cleanly is returned whole.
pub fn url_secrets(raw: &str) -> Vec<String> {
  let Some(url) = parse_clean(raw) else {
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
  if !user.is_empty() || !password.is_empty() {
    use base64::Engine as _;
    secrets.push(base64::engine::general_purpose::STANDARD.encode(format!(
      "{}:{}",
      percent_decode(user),
      percent_decode(password)
    )));
  }
  secrets.retain(|secret| !secret.is_empty());
  secrets.dedup();
  secrets
}

/// The user name of a cleanly parsed `user:password` value, as written and
/// percent-decoded — not a credential on its own. Empty for a lone `token@`
/// user name (that is the secret) and for values that do not parse cleanly.
pub fn public_username(raw: &str) -> Vec<String> {
  let Some(url) = parse_clean(raw) else {
    return Vec::new();
  };
  if url.username().is_empty() || url.password().is_none_or(str::is_empty) {
    return Vec::new();
  }
  vec![url.username().to_owned(), percent_decode(url.username())]
}

/// Decode `%XX` escapes (the form a proxy's Basic credentials are sent in);
/// a malformed escape is kept verbatim.
pub fn percent_decode(raw: &str) -> String {
  percent_encoding::percent_decode_str(raw)
    .decode_utf8_lossy()
    .into_owned()
}

/// Whether a proxy value is usable (hyper-util ignores one that does not
/// parse, falling back to `ALL_PROXY`).
fn usable(raw: &str) -> bool {
  !raw.is_empty() && parse_url(raw).is_some_and(|url| url.has_host())
}

/// [`parse_url`], refusing a value whose `@` does not end its userinfo: an
/// unencoded `/ ? #` in a password moves the credentials into the host, path,
/// query or fragment, where redaction and masking would miss them.
fn parse_clean(raw: &str) -> Option<Url> {
  let url = parse_url(raw)?;
  let has_userinfo = !url.username().is_empty() || url.password().is_some();
  let at_elsewhere = url.path().contains('@')
    || url.query().is_some_and(|query| query.contains('@'))
    || url
      .fragment()
      .is_some_and(|fragment| fragment.contains('@'));
  (!at_elsewhere && (has_userinfo || !raw.contains('@'))).then_some(url)
}

/// A leading `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )` followed by `://`.
fn has_scheme(raw: &str) -> bool {
  let Some((scheme, _)) = raw.split_once("://") else {
    return false;
  };
  let mut chars = scheme.chars();
  chars
    .next()
    .is_some_and(|first| first.is_ascii_alphabetic())
    && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}
