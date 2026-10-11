//! `CompleteJobRequest` / `StepResult` wire shape against the official
//! runner's contract (actions/runner cab9d1c `CompleteJobRequest.cs`,
//! `StepResult.cs`, serialized by `VssJsonMediaTypeFormatter`).

use std::collections::HashMap;
use std::error::Error;

use serde_json::{Value, json};
use wire::reporting::run_service::{CompleteJobRequest, JobConclusion};
use wire::reporting::{Annotation, ReportAnnotationLevel, StepResult, StepState};

type TestResult = Result<(), Box<dyn Error>>;

fn step(annotations: Vec<Annotation>) -> StepResult {
  StepResult {
    external_id: "a811e451-eda4-42c0-95d3-e04f5bf630c9".to_owned(),
    number: 2,
    name: "Run actions/checkout@v4".to_owned(),
    action_name: Some("actions/checkout".to_owned()),
    git_ref: Some("v4".to_owned()),
    kind: Some("node24".to_owned()),
    status: StepState::Completed,
    conclusion: JobConclusion::Succeeded,
    started_at: Some("2026-10-10T12:00:00+00:00".to_owned()),
    completed_at: Some("2026-10-10T12:00:03+00:00".to_owned()),
    completed_log_url: Some("https://results.example/step.log".to_owned()),
    completed_log_lines: Some(12),
    annotations,
  }
}

fn request(steps: Vec<StepResult>) -> CompleteJobRequest {
  CompleteJobRequest {
    plan_id: "be6a59be-77fa-40b8-ac53-4e5b8f08bfbb".to_owned(),
    job_id: "3a1bd763-6b39-5b52-b583-2f2433c96e56".to_owned(),
    request_id: 1,
    conclusion: JobConclusion::Succeeded,
    outputs: HashMap::new(),
    step_results: steps,
    annotations: Vec::new(),
    environment_url: Some("https://toolu-88.example/deploy".to_owned()),
    billing_owner_id: Some("U_kgDOAH-IyQ".to_owned()),
    infrastructure_failure_category: Some("resolve_action".to_owned()),
  }
}

fn keys(value: &Value) -> Vec<String> {
  let mut keys: Vec<String> = value
    .as_object()
    .map(|object| object.keys().cloned().collect())
    .unwrap_or_default();
  keys.sort();
  keys
}

#[test]
fn step_result_uses_upstream_snake_case_members_and_string_enums() -> TestResult {
  let value = serde_json::to_value(step(Vec::new()))?;
  assert_eq!(
    value,
    json!({
      "external_id": "a811e451-eda4-42c0-95d3-e04f5bf630c9",
      "number": 2,
      "name": "Run actions/checkout@v4",
      "action_name": "actions/checkout",
      "ref": "v4",
      "type": "node24",
      "status": "completed",
      "conclusion": "succeeded",
      "started_at": "2026-10-10T12:00:00+00:00",
      "completed_at": "2026-10-10T12:00:03+00:00",
      "completed_log_url": "https://results.example/step.log",
      "completed_log_lines": 12,
      "annotations": []
    })
  );
  Ok(())
}

#[test]
fn absent_step_metadata_is_omitted_but_annotations_are_always_sent() -> TestResult {
  let bare = StepResult {
    action_name: None,
    git_ref: None,
    kind: None,
    started_at: None,
    completed_at: None,
    completed_log_url: None,
    completed_log_lines: None,
    conclusion: JobConclusion::Failed,
    ..step(Vec::new())
  };
  let value = serde_json::to_value(bare)?;
  assert_eq!(
    keys(&value),
    [
      "annotations",
      "conclusion",
      "external_id",
      "name",
      "number",
      "status"
    ]
  );
  assert_eq!(value.get("annotations"), Some(&json!([])));
  assert_eq!(value.get("conclusion"), Some(&json!("failed")));
  // The toolu-only `outcome` and the old camelCase spellings are gone.
  for stale in ["outcome", "externalId", "completedLogURL", "startedAt"] {
    assert!(value.get(stale).is_none(), "{stale} must not be sent");
  }
  Ok(())
}

#[test]
fn every_task_result_name_matches_upstream() -> TestResult {
  for (conclusion, expected) in [
    (JobConclusion::Succeeded, "succeeded"),
    (JobConclusion::Failed, "failed"),
    (JobConclusion::Canceled, "canceled"),
    (JobConclusion::Skipped, "skipped"),
  ] {
    let value = serde_json::to_value(StepResult {
      conclusion,
      ..step(Vec::new())
    })?;
    assert_eq!(value.get("conclusion"), Some(&json!(expected)));
  }
  Ok(())
}

#[test]
fn step_annotations_keep_the_run_service_annotation_shape() -> TestResult {
  let annotation = Annotation {
    level: ReportAnnotationLevel::Failure,
    message: "Failed to resolve action download info.".to_owned(),
    title: None,
    raw_details: None,
    path: None,
    is_infrastructure_issue: true,
    start_line: 0,
    end_line: 0,
    start_column: 0,
    end_column: 0,
    step_number: 2,
  };
  let value = serde_json::to_value(step(vec![annotation]))?;
  assert_eq!(
    value.get("annotations"),
    Some(&json!([{
      "level": 3,
      "message": "Failed to resolve action download info.",
      "isInfrastructureIssue": true,
      "stepNumber": 2
    }]))
  );
  Ok(())
}

#[test]
fn completion_carries_environment_url_billing_owner_and_category() -> TestResult {
  let value = serde_json::to_value(request(vec![step(Vec::new())]))?;
  assert_eq!(
    keys(&value),
    [
      "annotations",
      "billingOwnerId",
      "conclusion",
      "environmentUrl",
      "infrastructureFailureCategory",
      "jobId",
      "outputs",
      "planId",
      "requestId",
      "stepResults"
    ]
  );
  assert_eq!(
    value.get("environmentUrl"),
    Some(&json!("https://toolu-88.example/deploy"))
  );
  assert_eq!(value.get("billingOwnerId"), Some(&json!("U_kgDOAH-IyQ")));
  assert_eq!(
    value.get("infrastructureFailureCategory"),
    Some(&json!("resolve_action"))
  );
  Ok(())
}

#[test]
fn absent_completion_fields_are_omitted_and_empty_url_is_sent() -> TestResult {
  let absent = CompleteJobRequest {
    environment_url: None,
    billing_owner_id: None,
    infrastructure_failure_category: None,
    ..request(Vec::new())
  };
  let value = serde_json::to_value(absent)?;
  for key in [
    "environmentUrl",
    "billingOwnerId",
    "infrastructureFailureCategory",
  ] {
    assert!(value.get(key).is_none(), "{key} must be omitted");
  }
  // Upstream's EmitDefaultValue=false omits only null: "" is still sent.
  let empty = CompleteJobRequest {
    environment_url: Some(String::new()),
    ..request(Vec::new())
  };
  assert_eq!(
    serde_json::to_value(empty)?.get("environmentUrl"),
    Some(&json!(""))
  );
  Ok(())
}
