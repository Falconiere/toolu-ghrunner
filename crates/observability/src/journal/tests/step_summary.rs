//! The journal stores summary metadata, never summary content.
use super::*;

#[test]
fn step_summary_journal_records_only_id_and_byte_size() -> Result<(), serde_json::Error> {
  let body = "# résumé\nprivate-summary-content\n";
  let event = JournalEvent::from(&RunnerEvent::StepSummary {
    step_id: "summary-id".to_owned(),
    content: body.to_owned(),
  });
  let json = serde_json::to_value(&event)?;
  assert_eq!(json.get("type"), Some(&serde_json::json!("step_summary")));
  assert_eq!(json.get("size"), Some(&serde_json::json!(body.len())));
  assert_eq!(json.get("step_id"), Some(&serde_json::json!("summary-id")));
  assert!(!serde_json::to_string(&event)?.contains("private-summary-content"));
  assert!(json.get("content").is_none());
  Ok(())
}
