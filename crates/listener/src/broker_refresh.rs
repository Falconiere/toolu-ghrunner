//! Bounded OAuth re-exchange for the broker's `ForceTokenRefresh` message.

use std::time::Duration;

use shared::RunnerError;

use crate::SessionCtx;

/// Maximum time spent sleeping between transient exchange attempts.
const REFRESH_RETRY_BUDGET: Duration = Duration::from_secs(30);
/// Maximum duration of one exchange request.
const REFRESH_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Sign a new JIT assertion and exchange it for a fresh broker access token.
/// The caller publishes the token only after the full exchange succeeds.
pub(crate) async fn refresh_access_token(ctx: &SessionCtx) -> Result<String, RunnerError> {
  let auth = ctx
    .refresh_auth
    .as_ref()
    .ok_or_else(|| RunnerError::Config("JIT refresh credentials unavailable".to_owned()))?;
  let jwt = protocol::build_jwt(
    &ctx.rsa_private_key_der,
    &auth.client_id,
    &auth.authorization_url,
  )?;

  let exchange = crate::retry::retry_transient(
    || async {
      match tokio::time::timeout(
        REFRESH_REQUEST_TIMEOUT,
        wire::net::exchange_token(&ctx.client, &auth.authorization_url, &jwt),
      )
      .await
      {
        Ok(result) => result,
        Err(_) => Err(RunnerError::Network("token exchange timed out".to_owned())),
      }
    },
    &ctx.cancel,
    REFRESH_RETRY_BUDGET,
    "broker_token_refresh",
  );
  let token = tokio::select! {
    biased;
    () = ctx.cancel.cancelled() => return Err(RunnerError::Cancelled),
    result = exchange => result?.access_token,
  };
  if token.is_empty() {
    return Err(RunnerError::Protocol(
      "token exchange returned an empty access token".to_owned(),
    ));
  }
  Ok(token)
}
