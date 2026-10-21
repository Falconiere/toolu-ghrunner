//! [`proxy_secrets`]: proxy credentials the secret masker must learn, and
//! [`url_credentials`], the same rule for any one credential-bearing URL.

use shared::NetworkConfig;

use crate::client::configured_proxy;
use crate::proxy_env::{ProxyEnv, ProxyVar, public_username, url_secrets};

/// Credential strings from the effective proxy URL(s) — user and password as
/// written and percent-decoded, `user:password`, and the
/// `Proxy-Authorization` credential `base64(user:password)` — via
/// [`url_secrets`], which fails safe: a value that does not parse cleanly
/// (scheme-less forms parse; an unencoded `/ ? #` in a password does not) is
/// registered whole, because steps receive it verbatim.
///
/// The user name of a `user:password` pair is not secret (upstream parity:
/// masking a name such as `proxy` would blank that word in every log line);
/// a lone `token@` user name is.
///
/// Config mode reads `[network] proxy`; env mode reads the runner's
/// `HTTPS_PROXY`, `HTTP_PROXY` and `ALL_PROXY` pairs exactly as reqwest does
/// ([`ProxyEnv`]), even under the CGI guard, since steps inherit them.
pub fn proxy_secrets(cfg: &NetworkConfig) -> Vec<String> {
  let raws: Vec<String> = if let Some(raw) = configured_proxy(cfg) {
    vec![raw.to_owned()]
  } else {
    let env = ProxyEnv::from_env();
    [env.https, env.http, env.all]
      .into_iter()
      .flatten()
      .map(|var: ProxyVar| var.value)
      .collect()
  };
  let mut seen = std::collections::HashSet::new();
  raws
    .iter()
    .flat_map(|raw| url_credentials(raw))
    .filter(|secret| seen.insert(secret.clone()))
    .collect()
}

/// The strings to mask for one URL that may carry credentials: its
/// [`url_secrets`] minus the user name of a `user:password` pair, which is
/// not secret (a lone `token@` user name is). Masking a name such as
/// `toolu` would blank that word in every line of the output.
pub fn url_credentials(raw: &str) -> Vec<String> {
  let public = public_username(raw);
  url_secrets(raw)
    .into_iter()
    .filter(|secret| !public.contains(secret))
    .collect()
}
