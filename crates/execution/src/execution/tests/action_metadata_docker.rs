//! Real Linux Docker metadata probes using the captured container job skeleton.

use super::{
  ALPINE, CAPTURE, TestResult, captured_job, collect_job, config_for, job_conclusion, linux_root,
  local_step, registry_step,
};
use shared::{ActionStep, AgentJobRequestMessage, Conclusion};
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[ignore = "requires Linux, real Docker, and a daemon-shared test root"]
async fn action_metadata_host_job_container_and_docker_actions() -> TestResult {
  for container_job in [false, true] {
    let root = linux_root()?;
    let config = config_for(root.path());
    let mut job = if container_job {
      serde_json::from_str::<AgentJobRequestMessage>(CAPTURE)?
    } else {
      captured_job()?
    };
    let workspace = config.workspace_root.join(&job.job_id);
    let local = workspace.join(".github/actions/metadata");
    tokio::fs::create_dir_all(&local).await?;
    tokio::fs::write(local.join("action.yml"), format!(
      "name: Metadata probe\ndescription: Real container environment probe.\nruns:\n  using: docker\n  image: docker://{ALPINE}\n  entrypoint: /bin/sh\n  args: ['/github/workspace/metadata-probe.sh']\n"
    )).await?;
    let script = r#"set -eu
printf '%s|%s|%s|%s\n' "$GITHUB_ACTION" "$GITHUB_ACTION_REPOSITORY" "$GITHUB_ACTION_REF" "$RUNNER_ENVIRONMENT" >> metadata.log
test "$RUNNER_ENVIRONMENT" = self-hosted
test -z "$GITHUB_ACTION_REPOSITORY"
test -z "$GITHUB_ACTION_REF"
"#;
    tokio::fs::write(workspace.join("metadata-probe.sh"), script).await?;
    let mut shell = ActionStep::script(
      "shell",
      &format!(
        "test '${{{{ runner.environment }}}}' = self-hosted\ntest '${{{{ github.action }}}}' = \"$GITHUB_ACTION\"\n{script}"
      ),
      "",
    );
    shell.name = Some("shell".to_owned());
    let mut registry = registry_step(
      "registry-scope",
      "/bin/sh",
      "/github/workspace/metadata-probe.sh",
    );
    registry.name = Some("__alpine".to_owned());
    let mut local = local_step("local-scope", "./.github/actions/metadata", &[]);
    local.name = Some("__self".to_owned());
    let mut after = shell.clone();
    after.id = "after".to_owned();
    after.name = Some("__run".to_owned());
    job.steps = vec![shell, registry, local, after];
    let events = collect_job(config, job, CancellationToken::new()).await?;
    assert_eq!(
      job_conclusion(&events),
      Some(Conclusion::Success),
      "{events:#?}"
    );
    assert_eq!(
      tokio::fs::read_to_string(workspace.join("metadata.log")).await?,
      "shell|||self-hosted\n__alpine|||self-hosted\n__self|||self-hosted\n__run|||self-hosted\n"
    );
  }
  Ok(())
}
