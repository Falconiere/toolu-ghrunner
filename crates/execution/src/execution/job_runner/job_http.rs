//! The job's HTTP clients and step network export, from `RunnerConfig.network`
//! (#94). Every client starts from `network::client_builder`, so the job's
//! proxy, bypass list and extra CA apply to all of them.

use std::time::Duration;

use shared::{RunnerConfig, RunnerError};

use crate::docker::container_spec::ContainerSpec;
use crate::docker::service_spec::ServiceSpec;
use crate::execution::context::ExecutionContext;

/// The one job-scope client `run_job` threads to every action API call,
/// node-runtime fetch and post-drain. A generous timeout: action tarballs
/// and node runtimes can be large.
///
/// # Errors
///
/// `RunnerError::Config` for an unusable `[network]`, or the TLS backend's
/// build error.
pub(super) fn build_job_client(config: &RunnerConfig) -> Result<reqwest::Client, RunnerError> {
  network::client_builder(&config.network)?
    .timeout(Duration::from_secs(120))
    .build()
    .map_err(|e| RunnerError::Network(format!("build job HTTP client: {e}")))
}

/// The accelerated cache's upstream (reverse-proxy) client: no total timeout,
/// as before — forwarded artifact bodies can be large.
///
/// # Errors
///
/// As [`build_job_client`].
pub(super) fn accelerated_upstream_client(
  config: &RunnerConfig,
) -> Result<reqwest::Client, RunnerError> {
  network::client_builder(&config.network)?
    .build()
    .map_err(|e| RunnerError::Network(format!("build accelerated upstream client: {e}")))
}

/// Mask the proxy credentials for the whole job and hand the host-step
/// export (proxy variables + `NODE_EXTRA_CA_CERTS`) to the context, which
/// layers it under workflow env for every host step.
pub(super) fn export_network(ctx: &mut ExecutionContext, config: &RunnerConfig) {
  ctx.add_masks(network::proxy_secrets(&config.network));
  ctx.set_network_env(network::step_env(&config.network).host);
}

/// Give the job container and every service container the proxy variables
/// (both casings, never `NODE_EXTRA_CA_CERTS`) unless their own `env` sets
/// them — upstream `ContainerInfo.UpdateWebProxyEnv`'s `TryAdd`. A `run:`
/// step `docker exec`'d into the job container inherits them from here.
pub(super) fn add_container_network_env(
  config: &RunnerConfig,
  job_container: Option<&mut ContainerSpec>,
  services: &mut [ServiceSpec],
) {
  let exported = network::step_env(&config.network).container;
  let specs = job_container
    .into_iter()
    .chain(services.iter_mut().map(|service| &mut service.container));
  for spec in specs {
    for (key, value) in &exported {
      if !spec.env.contains_key(key) {
        spec.env.insert(key.clone(), value.clone());
      }
    }
  }
}

#[cfg(test)]
#[path = "tests/job_http.rs"]
mod tests;
