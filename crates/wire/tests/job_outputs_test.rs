//! `CompleteJobRequest`'s job output wire shape from the issue 70 capture.

use std::collections::HashMap;

use wire::reporting::run_service::{CompleteJobOutput, CompleteJobRequest, JobConclusion};

#[test]
fn complete_job_outputs_are_value_objects_without_secret_flags() {
  let request = CompleteJobRequest {
    plan_id: "be6a59be-77fa-40b8-ac53-4e5b8f08bfbb".to_owned(),
    job_id: "3a1bd763-6b39-5b52-b583-2f2433c96e56".to_owned(),
    request_id: 1,
    conclusion: JobConclusion::Succeeded,
    outputs: HashMap::from([(
      "value".to_owned(),
      CompleteJobOutput {
        value: "hello-output".to_owned(),
      },
    )]),
    step_results: Vec::new(),
    annotations: Vec::new(),
  };
  let json = serde_json::to_value(request).unwrap();
  assert_eq!(
    json.get("conclusion"),
    Some(&serde_json::json!("succeeded"))
  );
  let outputs = json.get("outputs").unwrap();
  let value = outputs.get("value").unwrap();
  assert_eq!(
    outputs,
    &serde_json::json!({"value":{"value":"hello-output"}})
  );
  // Secret filtering belongs to the execution engine; this pins the wire object shape.
  assert!(value.get("isSecret").is_none());
}
