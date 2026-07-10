//! Real-data tests for the GHES V1 protocol path (AC #15).
//!
//! Drives `toolu_runner::net::v1::*` against a `wiremock` server
//! simulating a GHES instance. The mock returns the same shape the
//! V1 protocol spec uses:
//! - `GET /_apis/connectionData` → `ConnectionData` with service
//!   definitions (Timeline, Log Files).
//! - `POST /_apis/.../Timeline` → 200 OK with a timeline record.
//! - `GET /_apis/.../Timeline/{id}` → 200 OK with a record body.

use serde_json::Value;
use serde_json::json;
use wiremock::matchers::{bearer_token, body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn fetch_connection_data_returns_parsed_connection_data() {
  let server = MockServer::start().await;

  Mock::given(method("GET"))
    .and(path("/_apis/connectionData"))
    .and(bearer_token("ghes-token"))
    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
      "instanceId": "instance-1",
      "locationServiceData": {
        "serviceDefinitions": [
          {
            "identifier": protocol::v1::service_guids::TIMELINE,
            "serviceType": "Timeline",
            "displayName": "Timeline Service",
            "relativePath": "/_apis/distributedtask/hubs/Actions/Plans"
          },
          {
            "identifier": protocol::v1::service_guids::LOG_FILES,
            "serviceType": "LogFiles",
            "displayName": "Log Files Service",
            "relativePath": "/_apis/distributedtask/hubs/Actions/Logs"
          }
        ]
      }
    })))
    .expect(1)
    .mount(&server)
    .await;

  let client = reqwest::Client::new();
  let data = toolu_runner::net::v1::fetch_connection_data(&client, &server.uri(), "ghes-token")
    .await
    .expect("fetch connection data");
  assert_eq!(data.instance_id, "instance-1");
  assert_eq!(data.location_service_data.service_definitions.len(), 2);
}

#[tokio::test]
async fn fetch_connection_data_returns_protocol_error_on_http_failure() {
  let server = MockServer::start().await;

  Mock::given(method("GET"))
    .and(path("/_apis/connectionData"))
    .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
    .expect(1)
    .mount(&server)
    .await;

  let client = reqwest::Client::new();
  let err = toolu_runner::net::v1::fetch_connection_data(&client, &server.uri(), "bad-token")
    .await
    .expect_err("401 should error");
  let msg = format!("{err}");
  assert!(msg.contains("401"), "expected status in error: {msg}");
}

#[tokio::test]
async fn post_timeline_record_sends_record_with_bearer_auth() {
  let server = MockServer::start().await;

  Mock::given(method("POST"))
    .and(path("/_apis/distributedtask/hubs/Actions/Plans"))
    .and(bearer_token("ghes-timeline-token"))
    .and(body_partial_json(json!({
      "Id": "rec-1",
      "Type": "JobStarted"
    })))
    .respond_with(ResponseTemplate::new(200))
    .expect(1)
    .mount(&server)
    .await;

  let record = protocol::v1::TimelineRecord {
    id: "rec-1".to_owned(),
    parent_id: None,
    record_type: Some("JobStarted".to_owned()),
    name: None,
    state: None,
    result: None,
    start_time: Some("2026-06-18T12:00:00Z".to_owned()),
    finish_time: None,
    log: None,
    order: Some(1),
    error_count: None,
    warning_count: None,
  };

  let client = reqwest::Client::new();
  let timeline_url = format!("{}/_apis/distributedtask/hubs/Actions/Plans", server.uri());
  toolu_runner::net::v1::post_timeline_record(
    &client,
    &timeline_url,
    "ghes-timeline-token",
    &record,
  )
  .await
  .expect("post timeline record");
}

#[tokio::test]
async fn fetch_timeline_returns_parsed_record() {
  let server = MockServer::start().await;

  Mock::given(method("GET"))
    .and(path("/_apis/distributedtask/hubs/Actions/Plans/rec-7"))
    .and(bearer_token("ghes-token"))
    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
      "recordId": "rec-7",
      "type": "JobCompleted",
      "result": "succeeded"
    })))
    .expect(1)
    .mount(&server)
    .await;

  let client = reqwest::Client::new();
  let timeline_url = format!("{}/_apis/distributedtask/hubs/Actions/Plans", server.uri());
  let value = toolu_runner::net::v1::fetch_timeline(&client, &timeline_url, "ghes-token", "rec-7")
    .await
    .expect("fetch timeline record");
  let record_id = value.get("recordId").and_then(Value::as_str).unwrap_or("");
  let record_type = value.get("type").and_then(Value::as_str).unwrap_or("");
  let result = value.get("result").and_then(Value::as_str).unwrap_or("");
  let has_all = !record_id.is_empty() && !record_type.is_empty() && !result.is_empty();
  assert!(has_all, "missing fields in {value}");
  assert_eq!(record_id, "rec-7");
  assert_eq!(record_type, "JobCompleted");
  assert_eq!(result, "succeeded");
}

#[tokio::test]
async fn fetch_timeline_returns_protocol_error_on_404() {
  let server = MockServer::start().await;

  Mock::given(method("GET"))
    .and(path("/_apis/distributedtask/hubs/Actions/Plans/missing-id"))
    .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
    .expect(1)
    .mount(&server)
    .await;

  let client = reqwest::Client::new();
  let timeline_url = format!("{}/_apis/distributedtask/hubs/Actions/Plans", server.uri());
  let err =
    toolu_runner::net::v1::fetch_timeline(&client, &timeline_url, "ghes-token", "missing-id")
      .await
      .expect_err("404 should error");
  let msg = format!("{err}");
  assert!(msg.contains("404"), "expected status: {msg}");
  assert!(
    msg.contains("see debug log"),
    "expected redacted body msg: {msg}"
  );
}

#[tokio::test]
async fn ghes_v1_url_resolver_uses_known_guids() {
  // The pure URL resolvers (no I/O) live in `protocol::v1`. Exercise
  // them here end-to-end with the same data the wire mock would
  // return, to confirm the network and resolver agree on the URL shape.
  use protocol::v1::{
    ConnectionData, LocationServiceData, ServiceDefinition, resolve_service_url, service_guids,
  };

  let data = ConnectionData {
    instance_id: "instance-x".to_owned(),
    location_service_data: LocationServiceData {
      service_definitions: vec![ServiceDefinition {
        identifier: service_guids::TIMELINE.to_owned(),
        service_type: Some("Timeline".to_owned()),
        display_name: None,
        relative_path: Some("/_apis/distributedtask/hubs/Actions/Plans".to_owned()),
      }],
    },
  };

  let url = resolve_service_url("https://ghes.example.com", &data, service_guids::TIMELINE)
    .expect("timeline resolves");
  assert_eq!(
    url,
    "https://ghes.example.com/_apis/distributedtask/hubs/Actions/Plans"
  );
}
