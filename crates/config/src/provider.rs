//! The `[provider]` section of a registration (#206): where its jobs come
//! from.
//!
//! `kind = "github"` is the default and what a config without `[provider]`
//! loads as: `run` re-mints JIT configs from GitHub. A GitHub registration
//! writes no `[provider]` table, so its `config.toml` keeps its pre-#206
//! shape. `kind = "toolu"` is a machine paired with a toolu.sh organization:
//! `run` polls toolu.sh's `/runner/*` protocol for claimed JIT configs, and
//! the host token is the registration's `credentials.json` `access_token`.

use serde::{Deserialize, Serialize};

/// `[provider]`: the job source of this registration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum ProviderSection {
  /// A GitHub runner registration re-minted by `run`.
  #[default]
  Github,
  /// A toolu.sh machine polling `/runner/*`.
  Toolu {
    /// The toolu.sh API origin `/runner/*` is appended to, e.g.
    /// `https://console.toolu.sh`.
    api_url: String,
    /// The machine's host id on toolu.sh.
    host_id: String,
    /// The toolu.sh organization the machine is paired with.
    org: String,
  },
}

impl ProviderSection {
  /// Whether this is the default GitHub provider (serialized as no
  /// `[provider]`).
  pub fn is_github(&self) -> bool {
    matches!(self, ProviderSection::Github)
  }
}
