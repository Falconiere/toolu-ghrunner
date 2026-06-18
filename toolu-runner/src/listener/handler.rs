use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::job_lifecycle;
use crate::execution::secret_masker::SecretMasker;
use crate::net;
use protocol::JitConfig;
use protocol::auth::parse_rsa_private_key;
use shared::{ListenerEvent, RunnerConfig, RunnerError};

/// Shared state threaded through the listener lifecycle after authentication.
pub(crate) struct SessionCtx {
  pub(crate) client: reqwest::Client,
  pub(crate) token: String,
  pub(crate) broker_url: String,
  pub(crate) session_id: String,
  pub(crate) config: RunnerConfig,
  /// Shared with the tracing file sink's redactor and the
  /// `ExecutionContext` for each acquired job. Registrations from
  /// `register_secret` / `add_mask` flow through this Mutex to all
  /// readers, so secrets registered mid-job are redacted in the file
  /// sink and in the per-line upload channel without further wiring.
  pub(crate) masker: Arc<Mutex<SecretMasker>>,
  pub(crate) cancel: CancellationToken,
  pub(crate) tx: mpsc::Sender<ListenerEvent>,
}

/// GitHubListener wraps a Runner and handles the full GitHub protocol lifecycle:
/// decode JIT -> authenticate -> create session -> poll -> acquire -> execute -> report -> complete.
///
/// Per ERRATA #7/#13: This struct absorbs the broker role directly (no separate `broker.rs`).
pub struct GitHubListener {
  jit_config: JitConfig,
  client: reqwest::Client,
  config: RunnerConfig,
  masker: Arc<Mutex<SecretMasker>>,
}

impl GitHubListener {
  /// Create a new listener from a base64-encoded JIT config string.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Protocol` if JIT config parsing fails.
  pub fn new(
    jit_config_base64: &str,
    config: RunnerConfig,
    masker: Arc<Mutex<SecretMasker>>,
  ) -> Result<Self, RunnerError> {
    let jit_config = JitConfig::parse(jit_config_base64)?;
    let client = reqwest::Client::builder()
      .timeout(std::time::Duration::from_secs(60))
      .build()
      .map_err(|e| RunnerError::Protocol(format!("HTTP client build: {e}")))?;

    Ok(Self {
      jit_config,
      client,
      config,
      masker,
    })
  }

  /// Borrow the secret masker used to redact log output.
  pub fn masker(&self) -> &Arc<Mutex<SecretMasker>> {
    &self.masker
  }

  /// Borrow the runner config.
  pub fn config(&self) -> &RunnerConfig {
    &self.config
  }

  /// Run the listener lifecycle until cancellation or a fatal error.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Protocol` on auth or session creation failure.
  pub async fn run(&self, cancel: CancellationToken) -> Result<(), RunnerError> {
    let (tx, _rx) = mpsc::channel(256);
    let _der_bytes = parse_rsa_private_key(&self.jit_config.rsa_key_params)?;

    let jit = &self.jit_config;
    let client = &self.client;
    let config = &self.config;

    let token = net::authenticate(
      client,
      &jit.rsa_key_params,
      &jit.credentials.data.client_id,
      &jit.credentials.data.authorization_url,
    )
    .await?;

    let session_request = protocol::build_session_request(
      jit.runner_settings.agent_id,
      &jit.runner_settings.agent_name,
    );
    let session_response = net::create_session(
      client,
      &jit.runner_settings.server_url_v2,
      &token.access_token,
      &session_request,
    )
    .await?;

    let _ = tx
      .send(ListenerEvent::SessionCreated {
        session_id: session_response.session_id.clone(),
      })
      .await;

    let mut ctx = SessionCtx {
      client: client.clone(),
      token: token.access_token,
      broker_url: jit.runner_settings.server_url_v2.clone(),
      session_id: session_response.session_id,
      config: config.clone(),
      masker: Arc::clone(&self.masker),
      cancel,
      tx,
    };

    let result = job_lifecycle::poll_and_execute(&mut ctx).await;
    if let Err(ref e) = result {
      log_job_error(e);
    }

    super::helpers::cleanup_session(&ctx).await;
    result
  }
}

/// Log a job-execution error, distinguishing expected deregistration from real failures.
fn log_job_error(e: &RunnerError) {
  if is_runner_deregistered(e) {
    tracing::warn!(error = %e, "JIT runner deregistered before poll — likely duplicate mint or job reassigned");
  } else {
    tracing::error!(error = %e, "job execution failed");
  }
}

/// Check whether a runner error is the expected "Runner not found" from
/// GitHub's broker — happens when a JIT runner was deregistered before
/// it could poll (duplicate mint, job reassigned, or runner expired).
fn is_runner_deregistered(e: &RunnerError) -> bool {
  let msg = format!("{e}");
  msg.contains("status 404") && msg.contains("RunnerNotFound")
}
