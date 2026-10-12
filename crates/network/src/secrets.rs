//! [`proxy_secrets`]: proxy credentials the secret masker must learn.

use base64::Engine as _;
use shared::NetworkConfig;

use crate::client::{configured_proxy, parse_proxy};
use crate::config::{ALL_PROXY, HTTP_PROXY, HTTPS_PROXY, first_set};

/// Credential strings from the effective proxy URL(s): each password as
/// written and percent-decoded, and the `Proxy-Authorization` credential
/// `base64(user:password)`. A lone userinfo (`http://token@proxy`) is
/// treated as a token and registered too. The username of a `user:password`
/// pair is not secret (upstream parity).
///
/// Config mode reads `[network] proxy`; env mode reads the runner's
/// `HTTPS_PROXY`, `HTTP_PROXY` and `ALL_PROXY` values. Unparseable values contribute
/// nothing (a configured one already fails client construction).
pub fn proxy_secrets(cfg: &NetworkConfig) -> Vec<String> {
  let sources: Vec<String> = match configured_proxy(cfg) {
    Some(raw) => vec![raw.to_owned()],
    None => [HTTPS_PROXY, HTTP_PROXY, ALL_PROXY]
      .into_iter()
      .filter_map(first_set)
      .collect(),
  };
  let mut secrets: Vec<String> = Vec::new();
  let mut add = |value: String| {
    if !value.is_empty() && !secrets.contains(&value) {
      secrets.push(value);
    }
  };
  for raw in sources {
    let Ok(parsed) = parse_proxy(&raw) else {
      continue;
    };
    match (parsed.user, parsed.raw_password, parsed.password) {
      (user, Some(raw_password), Some(password)) => {
        let user = user.unwrap_or_default();
        add(base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}")));
        add(raw_password);
        add(password);
      },
      (Some(token), _, _) => {
        add(base64::engine::general_purpose::STANDARD.encode(format!("{token}:")));
        add(token);
        if let Some(raw_token) = parsed.raw_user {
          add(raw_token);
        }
      },
      (None, _, _) => {},
    }
  }
  secrets
}
