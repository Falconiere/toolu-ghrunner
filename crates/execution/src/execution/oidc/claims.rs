//! OIDC types and JWT claims construction.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// OIDC token mode -- how tokens are produced.
pub enum OidcMode {
  /// Proxy requests to GitHub's real OIDC provider.
  GitHub {
    /// The upstream OIDC URL from the `SystemVssConnection` endpoint.
    upstream_url: String,
  },
  /// Mint JWTs locally with a configurable signing key.
  Local {
    /// HS256 signing key bytes (at least 32 bytes).
    signing_key: Vec<u8>,
    /// Issuer URL for the `iss` claim.
    issuer_url: String,
  },
}

/// Configuration for the OIDC token service.
pub struct OidcConfig {
  /// Whether tokens are proxied to GitHub or minted locally.
  pub mode: OidcMode,
}

impl OidcConfig {
  /// Create a GitHub-proxy OIDC config.
  pub fn github(upstream_url: String) -> Self {
    Self {
      mode: OidcMode::GitHub { upstream_url },
    }
  }

  /// Create a local-mint OIDC config.
  pub fn local(signing_key: Vec<u8>, issuer_url: String) -> Self {
    Self {
      mode: OidcMode::Local {
        signing_key,
        issuer_url,
      },
    }
  }
}

/// Job context values needed to construct OIDC claims.
///
/// Extracted from the `github` context of the running job.
pub struct OidcJobContext {
  /// `owner/repo` of the running job.
  pub repository: String,
  /// Repository owner.
  pub repository_owner: String,
  /// User or app that triggered the workflow.
  pub actor: String,
  /// Event that triggered the workflow (`push`, `pull_request`, ...).
  pub event_name: String,
  /// The git ref the workflow ran on.
  pub git_ref: String,
  /// Commit SHA the workflow ran on.
  pub sha: String,
  /// Workflow name.
  pub workflow: String,
  /// GitHub-assigned run id.
  pub run_id: String,
  /// GitHub-assigned run number.
  pub run_number: String,
  /// Which attempt of the run this is.
  pub run_attempt: String,
}

/// Parameters for constructing OIDC claims.
pub struct OidcClaimsParams<'a> {
  /// Token issuer (`iss` claim).
  pub issuer: &'a str,
  /// Token subject (`sub` claim).
  pub subject: &'a str,
  /// Token audience (`aud` claim); defaults to `api://AzureADTokenExchange`.
  pub audience: Option<&'a str>,
  /// `owner/repo` of the running job.
  pub repository: &'a str,
  /// Repository owner.
  pub repository_owner: &'a str,
  /// User or app that triggered the workflow.
  pub actor: &'a str,
  /// Event that triggered the workflow (`push`, `pull_request`, ...).
  pub event_name: &'a str,
  /// The git ref the workflow ran on.
  pub git_ref: &'a str,
  /// Commit SHA the workflow ran on.
  pub sha: &'a str,
  /// Workflow name.
  pub workflow: &'a str,
  /// GitHub-assigned run id.
  pub run_id: &'a str,
  /// GitHub-assigned run number.
  pub run_number: &'a str,
  /// Which attempt of the run this is.
  pub run_attempt: &'a str,
}

/// JWT claims for OIDC tokens.
///
/// Follows GitHub Actions OIDC token format:
/// <https://docs.github.com/en/actions/deployment/security-hardening-your-deployments/about-security-hardening-with-openid-connect>
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OidcClaims {
  // Standard JWT claims
  /// Token issuer.
  pub iss: String,
  /// Token subject.
  pub sub: String,
  /// Token audience.
  pub aud: String,
  /// Expiry time (unix seconds); issued 10 minutes out.
  pub exp: u64,
  /// Not-before time (unix seconds).
  pub nbf: u64,
  /// Issued-at time (unix seconds).
  pub iat: u64,
  /// Unique token id (random UUID).
  pub jti: String,

  // GitHub-specific claims
  /// `owner/repo` of the running job.
  pub repository: String,
  /// Repository owner.
  pub repository_owner: String,
  /// User or app that triggered the workflow.
  pub actor: String,
  /// Event that triggered the workflow (`push`, `pull_request`, ...).
  pub event_name: String,
  /// The git ref the workflow ran on.
  #[serde(rename = "ref")]
  pub r#ref: String,
  /// Commit SHA the workflow ran on.
  pub sha: String,
  /// Workflow name.
  pub workflow: String,
  /// GitHub-assigned run id.
  pub run_id: String,
  /// GitHub-assigned run number.
  pub run_number: String,
  /// Which attempt of the run this is.
  pub run_attempt: String,
}

impl OidcClaims {
  /// Build OIDC claims from job context values.
  ///
  /// `audience` of `None` defaults to `api://AzureADTokenExchange`.
  pub fn new(params: &OidcClaimsParams<'_>) -> Self {
    let now = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map(|d| d.as_secs())
      .unwrap_or(0);

    Self {
      iss: params.issuer.to_owned(),
      sub: params.subject.to_owned(),
      aud: params
        .audience
        .unwrap_or("api://AzureADTokenExchange")
        .to_owned(),
      exp: now + 600, // 10 minutes
      nbf: now,
      iat: now,
      jti: Uuid::new_v4().to_string(),
      repository: params.repository.to_owned(),
      repository_owner: params.repository_owner.to_owned(),
      actor: params.actor.to_owned(),
      event_name: params.event_name.to_owned(),
      r#ref: params.git_ref.to_owned(),
      sha: params.sha.to_owned(),
      workflow: params.workflow.to_owned(),
      run_id: params.run_id.to_owned(),
      run_number: params.run_number.to_owned(),
      run_attempt: params.run_attempt.to_owned(),
    }
  }
}
