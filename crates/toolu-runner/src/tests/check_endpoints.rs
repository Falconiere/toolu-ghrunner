//! Tests for `check` endpoint derivation against the committed real-shaped
//! JIT fixtures and the upstream `ActionsCheck.cs` host table.

use url::Url;

use super::{Endpoint, JitEndpoints, Probe, endpoints_for, jit_endpoints, parse_target_url};

const GITHUB_COM_JIT: &str = include_str!("../../tests/fixtures/jit_config_github_com.json");
const GHES_JIT: &str = include_str!("../../tests/fixtures/jit_config_ghes.json");

/// Pull the `encoded` field out of a fixture without a JSON dependency in
/// the bin: the fixtures are `{ "host_kind": …, "encoded": "<b64>" }`.
fn encoded(fixture: &str) -> Option<String> {
  let start = fixture.find("\"encoded\"")?;
  let rest = fixture.get(start + "\"encoded\"".len()..)?;
  let open = rest.find('"')?;
  let rest = rest.get(open + 1..)?;
  let close = rest.find('"')?;
  rest.get(..close).map(str::to_owned)
}

fn url_of<'a>(endpoints: &'a [Endpoint], name: &str) -> Option<&'a str> {
  endpoints
    .iter()
    .find(|endpoint| endpoint.name == name)
    .and_then(|endpoint| match &endpoint.probe {
      Probe::Get(url) => Some(url.as_str()),
      Probe::Skip(_) => None,
    })
}

fn skip_of<'a>(endpoints: &'a [Endpoint], name: &str) -> Option<&'a str> {
  endpoints
    .iter()
    .find(|endpoint| endpoint.name == name)
    .and_then(|endpoint| match &endpoint.probe {
      Probe::Skip(reason) => Some(*reason),
      Probe::Get(_) => None,
    })
}

fn base(raw: &str) -> Result<Url, String> {
  parse_target_url(raw)
}

type TestResult = Result<(), String>;

#[test]
fn github_com_defaults_match_upstream_and_live_health_paths() -> TestResult {
  let endpoints = endpoints_for(&base("https://github.com/owner/repo")?, None);
  assert_eq!(url_of(&endpoints, "api"), Some("https://api.github.com/"));
  assert_eq!(
    url_of(&endpoints, "codeload"),
    Some("https://codeload.github.com/_ping")
  );
  assert_eq!(
    url_of(&endpoints, "pipelines"),
    Some("https://pipelines.actions.githubusercontent.com/_apis/health")
  );
  assert_eq!(
    url_of(&endpoints, "broker"),
    Some("https://broker.actions.githubusercontent.com/health")
  );
  assert_eq!(
    url_of(&endpoints, "token"),
    Some("https://vstoken.actions.githubusercontent.com/_apis/health")
  );
  assert_eq!(
    url_of(&endpoints, "results"),
    Some("https://results-receiver.actions.githubusercontent.com/health")
  );
  assert_eq!(
    url_of(&endpoints, "node-dist"),
    Some("https://nodejs.org/dist/")
  );
  let required: Vec<&str> = endpoints
    .iter()
    .filter(|endpoint| endpoint.required)
    .map(|endpoint| endpoint.name)
    .collect();
  assert_eq!(
    required,
    ["api", "codeload", "pipelines", "broker", "token", "results"]
  );
  Ok(())
}

#[test]
fn ghes_defaults_use_api_v3_and_services_paths_and_skip_results_and_broker() -> TestResult {
  let endpoints = endpoints_for(&base("https://ghes.example.com/my-org")?, None);
  assert_eq!(
    url_of(&endpoints, "api"),
    Some("https://ghes.example.com/api/v3")
  );
  assert_eq!(
    url_of(&endpoints, "codeload"),
    Some("https://codeload.ghes.example.com/_ping")
  );
  assert_eq!(
    url_of(&endpoints, "pipelines"),
    Some("https://ghes.example.com/_services/pipelines/_apis/health")
  );
  assert_eq!(
    url_of(&endpoints, "token"),
    Some("https://ghes.example.com/_services/vstoken/_apis/health")
  );
  assert!(skip_of(&endpoints, "results").is_some_and(|r| r.contains("GHES")));
  assert!(skip_of(&endpoints, "broker").is_some_and(|r| r.contains("V1")));
  Ok(())
}

#[test]
fn ip_literal_ghes_skips_codeload_and_keeps_the_http_scheme() -> TestResult {
  let endpoints = endpoints_for(&base("http://127.0.0.1:8080")?, None);
  assert_eq!(
    url_of(&endpoints, "api"),
    Some("http://127.0.0.1:8080/api/v3")
  );
  assert!(skip_of(&endpoints, "codeload").is_some_and(|r| r.contains("IP")));
  let v6 = endpoints_for(&base("http://[::1]:8080")?, None);
  assert!(skip_of(&v6, "codeload").is_some_and(|r| r.contains("IP")));
  Ok(())
}

#[test]
fn registration_endpoints_replace_pipelines_broker_and_token() -> TestResult {
  let jit: JitEndpoints =
    jit_endpoints(&encoded(GITHUB_COM_JIT).ok_or("fixture has no encoded field")?)?;
  assert_eq!(
    jit.pipelines,
    "https://pipelinesghubeus5.actions.githubusercontent.com/AbCdEfGhIjKlMnOpQrStUvWxYz12345678/"
  );
  assert_eq!(
    jit.broker,
    "https://vstst.action.github.com/AbCdEfGhIjKlMnOpQrStUvWxYz12345678/"
  );
  assert_eq!(jit.token, "https://github.com/login/oauth/access_token");
  let endpoints = endpoints_for(&base("https://github.com/owner/repo")?, Some(&jit));
  assert_eq!(
    url_of(&endpoints, "pipelines"),
    Some(jit.pipelines.as_str())
  );
  assert_eq!(url_of(&endpoints, "broker"), Some(jit.broker.as_str()));
  assert_eq!(url_of(&endpoints, "token"), Some(jit.token.as_str()));
  assert_eq!(url_of(&endpoints, "api"), Some("https://api.github.com/"));
  Ok(())
}

#[test]
fn ghes_v1_registration_with_equal_urls_skips_the_broker() -> TestResult {
  let jit = jit_endpoints(&encoded(GHES_JIT).ok_or("fixture has no encoded field")?)?;
  let endpoints = endpoints_for(&base("https://ghes.example.com")?, Some(&jit));
  assert_eq!(
    url_of(&endpoints, "pipelines"),
    Some("https://ghes.example.com/pipelines/_apis/distributedtask/hubs/Actions/Plans/")
  );
  assert!(skip_of(&endpoints, "broker").is_some());
  Ok(())
}

#[test]
fn ghe_com_tenants_are_hosted_like_github_com() -> TestResult {
  let endpoints = endpoints_for(&base("https://octo.ghe.com/org")?, None);
  assert_eq!(url_of(&endpoints, "api"), Some("https://api.octo.ghe.com/"));
  assert_eq!(
    url_of(&endpoints, "codeload"),
    Some("https://codeload.octo.ghe.com/_ping")
  );
  assert!(url_of(&endpoints, "results").is_some());
  Ok(())
}

#[test]
fn parse_target_url_rejects_non_http_and_hostless_values() {
  assert!(parse_target_url("ftp://github.com").is_err());
  assert!(parse_target_url("not a url").is_err());
  assert!(parse_target_url("https://").is_err());
  assert!(parse_target_url("https://GitHub.com/o/r").is_ok());
}

#[test]
fn a_garbage_jit_config_is_an_error_that_does_not_echo_the_value() -> TestResult {
  let err = jit_endpoints("s3cr3t-not-base64!!")
    .err()
    .ok_or("garbage parsed")?;
  assert!(!err.is_empty());
  assert!(!err.contains("s3cr3t"), "{err}");
  Ok(())
}
