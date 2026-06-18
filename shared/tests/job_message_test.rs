use shared::{
  ActionStep, AgentJobRequestMessage, DictEntry, PipelineContextData, TemplateToken,
};
use std::collections::HashMap;

#[test]
fn template_token_string() {
  let t = TemplateToken {
    token_type: 0,
    lit: Some("hello".to_owned()),
    ..TemplateToken::default()
  };
  assert_eq!(t.to_string_value(), Some("hello"));
}

#[test]
fn template_token_expr() {
  let t = TemplateToken {
    token_type: 3,
    expr: Some("${{ github.event }}".to_owned()),
    ..TemplateToken::default()
  };
  assert_eq!(t.to_expr_string(), Some("${{ github.event }}"));
}

#[test]
fn template_token_to_map() {
  let t = TemplateToken {
    token_type: 2,
    d: Some(vec![
      DictEntry {
        key: TemplateToken {
          token_type: 0,
          lit: Some("name".to_owned()),
          ..TemplateToken::default()
        },
        value: TemplateToken {
          token_type: 0,
          lit: Some("alice".to_owned()),
          ..TemplateToken::default()
        },
      },
      DictEntry {
        key: TemplateToken {
          token_type: 0,
          lit: Some("age".to_owned()),
          ..TemplateToken::default()
        },
        value: TemplateToken {
          token_type: 6,
          num_val: Some(30.0),
          ..TemplateToken::default()
        },
      },
    ]),
    ..TemplateToken::default()
  };
  let m = t.to_map();
  assert_eq!(
    m.get("name").and_then(|v| v.to_string_value()),
    Some("alice")
  );
  assert_eq!(
    m.get("age").and_then(|v| v.num_val),
    Some(30.0),
  );
}

#[test]
fn pipeline_context_data_constructors() {
  let s = PipelineContextData::string("foo".to_owned());
  assert_eq!(s.data_type, 0);
  assert_eq!(s.s.as_deref(), Some("foo"));
  assert!(s.b.is_none());

  let b = PipelineContextData::bool(true);
  assert_eq!(b.data_type, 3);
  assert_eq!(b.b, Some(true));

  let n = PipelineContextData::number(2.5);
  assert_eq!(n.data_type, 4);
  assert_eq!(n.n, Some(2.5));

  let null = PipelineContextData::null();
  assert_eq!(null.data_type, 5);
  assert!(null.s.is_none() && null.b.is_none() && null.n.is_none());
}

#[test]
fn pipeline_context_data_deserializes_bare_string() {
  let json = r#""hello""#;
  let p: PipelineContextData = serde_json::from_str(json).unwrap();
  assert_eq!(p.data_type, 0);
  assert_eq!(p.s.as_deref(), Some("hello"));
}

#[test]
fn pipeline_context_data_deserializes_bare_bool() {
  let p: PipelineContextData = serde_json::from_str("true").unwrap();
  assert_eq!(p.data_type, 3);
  assert_eq!(p.b, Some(true));
}

#[test]
fn pipeline_context_data_deserializes_typed_object() {
  let json = r#"{"type":0,"s":"world"}"#;
  let p: PipelineContextData = serde_json::from_str(json).unwrap();
  assert_eq!(p.data_type, 0);
  assert_eq!(p.s.as_deref(), Some("world"));
}

#[test]
fn action_step_script_constructor() {
  let step = ActionStep::script("step-1", "echo hi", "always()");
  assert_eq!(step.id, "step-1");
  assert_eq!(step.script_body().as_deref(), Some("echo hi"));
  assert!(step.is_run_step());
  assert_eq!(step.runs_using(), Some("script"));
  assert_eq!(step.condition.as_deref(), Some("always()"));
}

#[test]
fn action_step_script_with_empty_condition() {
  let step = ActionStep::script("s", "ls", "");
  assert!(step.condition.is_none());
}

#[test]
fn action_step_with_ref_type() {
  let step = ActionStep::with_ref_type("checkout", "node20");
  assert_eq!(step.id, "checkout");
  assert_eq!(step.runs_using(), Some("node20"));
  assert!(!step.is_run_step());
}

#[test]
fn action_step_input_lookup() {
  let step = ActionStep::with_ref_type("setup-node", "node20");
  // No inputs set; should return None.
  assert!(step.input("node-version").is_none());
}

#[test]
fn agent_job_request_message_minimal_deserialize() {
  // Minimal real-shape job message — matches the C# runner's JSON.
  let json = r#"{
    "messageType": "JobRequest",
    "plan": { "planId": "plan-1" },
    "jobId": "job-abc",
    "jobDisplayName": "build",
    "jobName": "build"
  }"#;
  let job: AgentJobRequestMessage = serde_json::from_str(json).unwrap();
  assert_eq!(job.job_id, "job-abc");
  assert_eq!(job.job_name, "build");
  assert_eq!(job.plan.plan_id, "plan-1");
  assert!(job.steps.is_empty());
  assert!(job.run_service_url().is_none());
  assert!(job.server_url().is_none());
}

#[test]
fn agent_job_request_message_with_run_service_url() {
  let json = r#"{
    "messageType": "JobRequest",
    "plan": { "planId": "p" },
    "jobId": "j",
    "jobDisplayName": "j",
    "jobName": "j",
    "runServiceUrl": "https://run.service.example.com"
  }"#;
  let job: AgentJobRequestMessage = serde_json::from_str(json).unwrap();
  assert_eq!(
    job.run_service_url().map(String::as_str),
    Some("https://run.service.example.com")
  );
}

#[test]
fn agent_job_request_message_with_context_data() {
  let mut json = std::collections::HashMap::new();
  json.insert(
    "messageType".to_owned(),
    serde_json::Value::String("JobRequest".to_owned()),
  );
  json.insert(
    "plan".to_owned(),
    serde_json::json!({ "planId": "p" }),
  );
  json.insert("jobId".to_owned(), serde_json::Value::String("j".to_owned()));
  json.insert(
    "jobDisplayName".to_owned(),
    serde_json::Value::String("j".to_owned()),
  );
  json.insert("jobName".to_owned(), serde_json::Value::String("j".to_owned()));
  json.insert(
    "contextData".to_owned(),
    serde_json::json!({
      "github": { "type": 0, "s": "value" },
      "numeric": { "type": 4, "n": 42.0 }
    }),
  );

  let job: AgentJobRequestMessage = serde_json::from_value(serde_json::Value::Object(
    json.into_iter().collect(),
  ))
  .unwrap();
  let ctx = job.context_data;
  assert_eq!(ctx.len(), 2);
  let github = ctx.get("github").unwrap();
  assert_eq!(github.s.as_deref(), Some("value"));
  let numeric = ctx.get("numeric").unwrap();
  assert_eq!(numeric.n, Some(42.0));
}

#[test]
fn _check_compiles() {
  // dummy smoke check that the HashMap import isn't accidentally dropped
  let _: HashMap<String, i32> = HashMap::new();
}
