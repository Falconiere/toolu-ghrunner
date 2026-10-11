//! Report serialization using a wire ID from the sanitized acquired job.
//!
//! The event sequence is derived from the captured step. It proves the local
//! reporting path and does not stand in for a live Results Service response.

use std::collections::HashMap;
use std::error::Error;

use shared::{AgentJobRequestMessage, Conclusion, RunnerEvent};
use wire::reporting::StepState;
use wire::reporting::run_service::JobConclusion;

use crate::step_report_queue::{StepMetaMap, build_step_entry};
use crate::step_reporter::StepCollector;

const CAPTURED_JOB: &str = include_str!(concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/../toolu-runner/tests/fixtures/job_message.json"
));

#[tokio::test]
async fn skipped_step_reports_captured_wire_id_and_skipped_conclusion() -> Result<(), Box<dyn Error>>
{
  let message: AgentJobRequestMessage = serde_json::from_str(CAPTURED_JOB)?;
  let step = message.steps.get(1).ok_or("captured step missing")?;
  let start = RunnerEvent::StepStarted {
    step_id: step.id.clone(),
    step_name: "Checkout".to_owned(),
    step_number: 2,
  };
  let skipped = RunnerEvent::StepSkipped {
    step_id: step.id.clone(),
    reason: "condition evaluated to false".to_owned(),
  };
  let finish = RunnerEvent::StepCompleted {
    step_id: step.id.clone(),
    conclusion: Conclusion::Skipped,
    outputs: HashMap::new(),
  };

  let mut meta = StepMetaMap::new();
  let collector = StepCollector::new();
  for event in [&start, &skipped, &finish] {
    if let Some(entry) = build_step_entry(event, &mut meta) {
      let json = serde_json::to_value(entry)?;
      assert_eq!(json.get("external_id"), Some(&serde_json::json!(step.id)));
      if matches!(event, RunnerEvent::StepCompleted { .. }) {
        assert_eq!(json.get("status"), Some(&serde_json::json!(6)));
        assert_eq!(json.get("conclusion"), Some(&serde_json::json!(7)));
      }
    }
    collector.record(event).await;
  }

  let results = collector.collected_results().await;
  let result = results.first().ok_or("missing completed result")?;
  assert_eq!(results.len(), 1);
  assert_eq!(result.external_id, step.id);
  assert_eq!(result.number, 2);
  assert_eq!(result.name, "Checkout");
  assert_eq!(result.status, StepState::Completed);
  assert_eq!(result.conclusion, JobConclusion::Skipped);
  let json = serde_json::to_value(result)?;
  assert_eq!(json.get("external_id"), Some(&serde_json::json!(step.id)));
  assert_eq!(json.get("conclusion"), Some(&serde_json::json!("skipped")));
  Ok(())
}
