//! The `[auth]` section of a registration (#201): which bearer `register`
//! and the always-online re-mint send to `generate-jitconfig`.
//!
//! `kind = "token"` is the default and what a config without `[auth]` loads
//! as: re-mint resolves `TOOLU_RUNNER_TOKEN` or the stored `login` token, as
//! before. A token registration writes no `[auth]` table, so its
//! `config.toml` keeps its pre-#201 shape. `kind = "app"` mints a GitHub App
//! installation token from the App saved in `app_file` for `installation_id`.

use serde::{Deserialize, Serialize};

/// `[auth]`: how this registration obtains its REST bearer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum AuthSection {
  /// A personal token: `TOOLU_RUNNER_TOKEN` or the stored `login` token.
  #[default]
  Token,
  /// A GitHub App installation token minted per re-mint.
  App {
    /// The App file `create-app` saved (`<home>/github-app.json`, 0600).
    app_file: String,
    /// The installation of the App on this registration's repo or org.
    installation_id: i64,
  },
}

impl AuthSection {
  /// Whether this is the default token auth (serialized as no `[auth]`).
  pub fn is_token(&self) -> bool {
    matches!(self, AuthSection::Token)
  }
}
