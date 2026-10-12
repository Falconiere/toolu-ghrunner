//! Proxy URL redaction: the one way runner-authored text shows a proxy.
//!
//! A proxy URL may carry `user:password@` credentials (upstream
//! `RunnerWebProxy` accepts the same shape), so every message, `Debug` and
//! diagnostic prints it through [`redact_proxy_url`]. Pure string handling —
//! it must also redact values that do not parse as URLs.

/// Replace the credential part of a proxy URL with `***`.
///
/// `http://user:pass@host:3128` → `http://user:***@host:3128`; a bare
/// `http://token@host` → `http://***@host` (a lone userinfo is often a token);
/// a value without credentials is returned unchanged. Works without a scheme
/// (`user:pass@host` → `user:***@host`), because a rejected config value is
/// echoed through here too.
pub fn redact_proxy_url(raw: &str) -> String {
  let (scheme, rest) = raw
    .split_once("://")
    .map_or(("", raw), |(scheme, rest)| (scheme, rest));
  let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
  let (authority, tail) = rest.split_at(authority_end);
  let Some((userinfo, host)) = authority.rsplit_once('@') else {
    return raw.to_owned();
  };
  let redacted = match userinfo.split_once(':') {
    Some((user, _password)) if !user.is_empty() => format!("{user}:***"),
    Some(_) | None => "***".to_owned(),
  };
  let prefix = if scheme.is_empty() {
    String::new()
  } else {
    format!("{scheme}://")
  };
  format!("{prefix}{redacted}@{host}{tail}")
}
