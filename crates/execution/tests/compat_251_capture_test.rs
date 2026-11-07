//! #251: the sanitized live `stack` probe message from paired run 42424784091
//! (toolu lane, Linux-X64) decodes into the engine's job request with its wire
//! shape intact: the job container is an unevaluated expression, two service
//! containers, a registry-image step and a pinned remote Dockerfile action.

use execution::execution::action_exec::build_uses_ref;
use shared::{AgentJobRequestMessage, TemplateToken};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const CAPTURE: &str = include_str!("compat_251_stack_message.json");

fn captured_job() -> Result<AgentJobRequestMessage, serde_json::Error> {
  serde_json::from_str(CAPTURE)
}

fn map_value<'a>(map: &'a TemplateToken, key: &str) -> Option<&'a TemplateToken> {
  map
    .d
    .as_ref()?
    .iter()
    .find_map(|entry| (entry.key.to_string_value() == Some(key)).then_some(&entry.value))
}

#[test]
fn stack_capture_keeps_its_job_and_step_identities() -> TestResult {
  let job = captured_job()?;
  assert_eq!(job.job_id, "4cb56773-e12f-5d30-b011-369a51cf8479");
  assert_eq!(job.job_display_name, "probe-Linux-X64-toolu / stack");
  let names: Vec<_> = job
    .steps
    .iter()
    .map(|step| step.context_name.as_deref().unwrap_or_default())
    .collect();
  assert_eq!(
    names,
    [
      "__actions_checkout",
      "__run",
      "__run_2",
      "pulled",
      "built",
      "__run_3",
      "payloads",
      "__actions_cache",
      "__actions_upload-artifact",
      "__run_4",
    ]
  );
  let pulled = job.steps.get(3).ok_or("no pulled step")?;
  assert_eq!(
    pulled.reference.ref_type.as_deref(),
    Some("containerRegistry")
  );
  assert_eq!(
    pulled.reference.image.as_deref(),
    Some("alpine@sha256:d9e853e87e55526f6b2917df91a2115c36dd7c696a35be12163d44e6e2a4b6bc")
  );
  let built = job.steps.get(4).ok_or("no built step")?;
  assert_eq!(built.reference.repository_type.as_deref(), Some("GitHub"));
  assert_eq!(
    build_uses_ref(&built.reference),
    "Falconiere/toolu-ghrunner/.github/actions/compat-251-docker@fd0192158b9f966705feed24efb1d912bbf20fab"
  );
  Ok(())
}

#[test]
fn stack_capture_keeps_the_container_expression_and_services() -> TestResult {
  let job = captured_job()?;
  let container = job.job_container.as_ref().ok_or("no jobContainer")?;
  // The image is chosen per variant on the runner, so it arrives unevaluated.
  assert_eq!(container.token_type, 3);
  assert_eq!(
    container.expr.as_deref(),
    Some(
      "inputs.who == 'world' && 'ubuntu@sha256:fe81560990fae86dc4743202b2cb4d3df98d0bcf949173cecef15ce1127ef852' || 'ubuntu@sha256:0000000000000000000000000000000000000000000000000000000000000251'"
    )
  );
  let services = job
    .job_service_containers
    .as_ref()
    .ok_or("no jobServiceContainers")?;
  let redis = map_value(services, "redis").ok_or("no redis service")?;
  assert_eq!(
    map_value(redis, "image").and_then(TemplateToken::to_string_value),
    Some("redis@sha256:c5d34a98396a4775e223c74e929162c35c5a2182901ba26b50ade939941ce14b")
  );
  assert!(map_value(services, "nginx").is_some(), "no nginx service");
  Ok(())
}
