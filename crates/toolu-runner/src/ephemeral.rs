//! End of an `--ephemeral` registration's single job (#92).
//!
//! Upstream's `--ephemeral` lets the service remove the runner after its job
//! and then deletes the local `.runner` / `.credentials`. A toolu
//! registration is a JIT runner, which GitHub also removes after one job, so
//! this module verifies that instead of assuming it: with a bearer
//! (`TOOLU_RUNNER_TOKEN` > stored `login`) it calls the same idempotent
//! unregister as `remove` (DELETE on the repo or org runners API, a 404
//! confirmed by a lookup). Without a bearer — or for a URL no runners API can
//! address — the local state still goes and the output says the GitHub side
//! was not verified. A
//! retryable unregister failure keeps the local state (the persisted
//! `runner_id` is the only handle) and fails naming `toolu-runner remove`.

use std::path::Path;

use config::config::RunnerRegistrationConfig;
use shared::RunnerError;

/// What toolu could establish about the GitHub-side registration.
enum GithubSide {
  /// The unregister call succeeded (or confirmed the runner already gone).
  Unregistered,
  /// No call was possible; GitHub removes a used JIT runner on its own.
  Unverified(&'static str),
}

/// Paths of the registration's local state.
pub(crate) struct LocalState<'a> {
  /// The registration's `config.toml`.
  pub(crate) config: &'a Path,
  /// The `credentials.json` beside it.
  pub(crate) creds: &'a Path,
  /// The `.pending_remove` marker.
  pub(crate) pending: &'a Path,
  /// The job `.lock` this `run` holds.
  pub(crate) lock: &'a Path,
}

/// Unregister the ephemeral runner after its job and delete the local
/// registration (`_diag/` kept).
///
/// # Errors
///
/// A retryable unregister failure (network, 5xx, rejected token): nothing
/// local is deleted and the message names `toolu-runner remove`. A failure to
/// delete `config.toml` itself is also returned.
pub(crate) async fn finish_job(
  cfg: &RunnerRegistrationConfig,
  state: &LocalState<'_>,
  client: &reqwest::Client,
) -> Result<(), Box<dyn std::error::Error>> {
  let github = unregister(cfg, client).await?;
  crate::delete_registration_state(state.config, state.creds, state.pending, state.lock, false)
    .map_err(|e| {
      format!(
        "ephemeral runner '{}' finished its job and GitHub was handled, but deleting {} failed: \
         {e}",
        cfg.runner_name,
        state.config.display()
      )
    })?;
  let summary = match github {
    GithubSide::Unregistered => "unregistered on GitHub".to_owned(),
    GithubSide::Unverified(why) => format!(
      "GitHub removes a used JIT runner itself — not verified here ({why}); check the \
       repository's runner list"
    ),
  };
  println!(
    "ephemeral runner '{}' finished its job: {summary}; local registration removed (_diag kept).",
    cfg.runner_name
  );
  Ok(())
}

/// Call GitHub's unregister when a bearer is available.
async fn unregister(
  cfg: &RunnerRegistrationConfig,
  client: &reqwest::Client,
) -> Result<GithubSide, Box<dyn std::error::Error>> {
  let Some(token) = crate::remove_bearer(&cfg.runner_url, None)? else {
    // The unregister takes a personal token even for an App registration.
    return Ok(GithubSide::Unverified(if cfg.auth.is_token() {
      "no GitHub token"
    } else {
      "no personal GitHub token; an App registration's unregister still needs one"
    }));
  };
  let outcome = wire::net::unregister_runner(
    client,
    &cfg.runner_url,
    &token,
    cfg.runner_id,
    &cfg.runner_name,
    crate::UNREGISTER_TIMEOUT,
  )
  .await;
  match outcome {
    Ok(()) => Ok(GithubSide::Unregistered),
    Err(RunnerError::Config(msg)) => {
      tracing::warn!(error = %msg, "cannot address a runners API endpoint for this registration");
      Ok(GithubSide::Unverified(
        "no runners API endpoint for this URL",
      ))
    },
    Err(e) => Err(
      format!(
        "ephemeral runner '{}' finished its job, but unregistering it on GitHub failed: {e}\n\
         The local registration was kept — fix the token or connectivity, then run \
         `toolu-runner remove` to retry.",
        cfg.runner_name
      )
      .into(),
    ),
  }
}
