//! OIDC types and JWT claims construction.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// OIDC token mode -- how tokens are produced.
pub enum OidcMode {
  /// Proxy requests to GitHub's real OIDC provider.
  GitHub {
    /// The upstream OIDC URL from SystemVssConnection endpoint.
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
  pub repository: String,
  pub repository_owner: String,
  pub actor: String,
  pub event_name: String,
  pub git_ref: String,
  pub sha: String,
  pub workflow: String,
  pub run_id: String,
  pub run_number: String,
  pub run_attempt: String,
}

/// Parameters for constructing OIDC claims.
pub struct OidcClaimsParams<'a> {
  pub issuer: &'a str,
  pub subject: &'a str,
  pub audience: Option<&'a str>,
  pub repository: &'a str,
  pub repository_owner: &'a str,
  pub actor: &'a str,
  pub event_name: &'a str,
  pub git_ref: &'a str,
  pub sha: &'a str,
  pub workflow: &'a str,
  pub run_id: &'a str,
  pub run_number: &'a str,
  pub run_attempt: &'a str,
}

/// JWT claims for OIDC tokens.
///
/// Follows GitHub Actions OIDC token format:
/// <https://docs.github.com/en/actions/deployment/security-hardening-your-deployments/about-security-hardening-with-openid-connect>
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OidcClaims {
  // Standard JWT claims
  pub iss: String,
  pub sub: String,
  pub aud: String,
  pub exp: u64,
  pub nbf: u64,
  pub iat: u64,
  pub jti: String,

  // GitHub-specific claims
  pub repository: String,
  pub repository_owner: String,
  pub actor: String,
  pub event_name: String,
  #[serde(rename = "ref")]
  pub r#ref: String,
  pub sha: String,
  pub workflow: String,
  pub run_id: String,
  pub run_number: String,
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
