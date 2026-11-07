//! Capture-driven Docker setup discovery checks.

use super::*;

#[tokio::test]
async fn duplicate_captured_registry_image_needs_one_pull() -> Result<(), Box<dyn std::error::Error>>
{
  let job: shared::AgentJobRequestMessage = serde_json::from_str(include_str!(
    "../../../tests/compat_103_docker_message.json"
  ))?;
  let step = job
    .steps
    .iter()
    .find(|step| step.reference.ref_type.as_deref() == Some("containerRegistry"))
    .ok_or("captured Docker step missing")?;
  let mut duplicate = step.clone();
  duplicate.reference.image = duplicate
    .reference
    .image
    .map(|image| image.to_ascii_uppercase());
  let fetcher = ActionFetcher::new(&shared::NetworkConfig::default())?;
  let plan = discover(
    &[step.clone(), duplicate],
    &Discovery {
      fetcher: &fetcher,
      client: &reqwest::Client::new(),
      data_dir: Path::new(""),
    },
    &CancellationToken::new(),
  )
  .await?;
  assert_eq!(
    plan.pulls,
    vec![step.reference.image.clone().ok_or("image")?]
  );
  assert!(plan.builds.is_empty());
  Ok(())
}
