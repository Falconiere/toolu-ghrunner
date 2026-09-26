//! Tests for typed job-container declaration evaluation.

use std::collections::HashMap;

use expressions::types::ExprValue;
use shared::{DictEntry, TemplateToken};

use super::{ContainerSpec, ExecutionContext};

fn literal(value: &str) -> TemplateToken {
  TemplateToken {
    token_type: 0,
    lit: Some(value.to_owned()),
    ..Default::default()
  }
}

fn expression(value: &str) -> TemplateToken {
  TemplateToken {
    token_type: 3,
    expr: Some(value.to_owned()),
    ..Default::default()
  }
}

fn mapping(entries: Vec<(&str, TemplateToken)>) -> TemplateToken {
  TemplateToken {
    token_type: 2,
    d: Some(
      entries
        .into_iter()
        .map(|(key, value)| DictEntry {
          key: literal(key),
          value,
        })
        .collect(),
    ),
    ..Default::default()
  }
}

fn sequence(entries: Vec<TemplateToken>) -> TemplateToken {
  TemplateToken {
    token_type: 1,
    seq: Some(entries),
    ..Default::default()
  }
}

/// A real workflow accepts the compact image-only `container: alpine:3.20`
/// form and interpolation inside the image reference.
#[test]
fn evaluates_the_image_shorthand_with_expression_interpolation() {
  let mut ctx = ExecutionContext::new_for_test();
  ctx.set_env("IMAGE_TAG", "3.20");
  let token = literal("alpine:${{ env.IMAGE_TAG }}");

  let result = ContainerSpec::evaluate(Some(&token), &ctx);
  assert!(
    matches!(result, Ok(Some(_))),
    "image shorthand must evaluate: {result:?}"
  );
  if let Ok(Some(spec)) = result {
    assert_eq!(
      spec,
      ContainerSpec {
        image: "alpine:3.20".to_owned(),
        credentials: None,
        env: HashMap::new(),
        ports: Vec::new(),
        volumes: Vec::new(),
        options: Vec::new(),
      }
    );
  }
}

/// Object declarations recursively resolve strings, lists, maps, credentials,
/// and the quote-aware create-options field before Docker lifecycle code runs.
#[test]
fn evaluates_a_complete_typed_object_declaration() {
  let mut ctx = ExecutionContext::new_for_test();
  ctx.set_env("TAG", "bookworm");
  ctx.register_secret("REGISTRY_PASSWORD", "registry-password-value");
  let token = mapping(vec![
    ("image", literal("debian:${{ env.TAG }}")),
    (
      "credentials",
      mapping(vec![
        ("username", literal("ci-user")),
        ("password", expression("secrets.REGISTRY_PASSWORD")),
      ]),
    ),
    ("env", mapping(vec![("APP_MODE", literal("test"))])),
    (
      "ports",
      sequence(vec![literal("8080"), literal("8443/tcp")]),
    ),
    ("volumes", sequence(vec![literal("cache:/var/cache/app")])),
    ("options", literal("--cpus 2 --hostname 'ci worker'")),
  ]);

  match ContainerSpec::evaluate(Some(&token), &ctx) {
    Ok(Some(spec)) => {
      assert_eq!(spec.image, "debian:bookworm");
      assert_eq!(spec.env.get("APP_MODE"), Some(&"test".to_owned()));
      assert_eq!(spec.ports, vec!["8080", "8443/tcp"]);
      assert_eq!(spec.volumes, vec!["cache:/var/cache/app"]);
      assert_eq!(spec.options, vec!["--cpus", "2", "--hostname", "ci worker"]);
      match spec.credentials {
        Some(credentials) => {
          assert_eq!(credentials.username, "ci-user");
          assert_eq!(credentials.password, "registry-password-value");
        },
        None => assert!(spec.credentials.is_some(), "credentials must be retained"),
      }
    },
    other => assert!(
      matches!(other, Ok(Some(_))),
      "the declaration must evaluate: {other:?}"
    ),
  }
}

/// A whole expression can produce the object form because GitHub emits
/// expression tokens for dynamic job declarations.
#[test]
fn evaluates_an_expression_produced_object() {
  let mut ctx = ExecutionContext::new_for_test();
  let mut container = HashMap::new();
  container.insert(
    "image".to_owned(),
    ExprValue::String("alpine:3.20".to_owned()),
  );
  container.insert(
    "ports".to_owned(),
    ExprValue::array(vec![ExprValue::String("3000".to_owned())]),
  );
  ctx.set_github_context_value("container_declaration", ExprValue::object(container));

  match ContainerSpec::evaluate(Some(&expression("github.container_declaration")), &ctx) {
    Ok(Some(spec)) => {
      assert_eq!(spec.image, "alpine:3.20");
      assert_eq!(spec.ports, vec!["3000"]);
    },
    other => assert!(
      matches!(other, Ok(Some(_))),
      "object expression must evaluate: {other:?}"
    ),
  }
}

/// Missing and explicit null declarations leave the job on the regular host
/// lifecycle path.
#[test]
fn missing_or_null_declaration_is_absent() {
  let ctx = ExecutionContext::new_for_test();
  let null = TemplateToken {
    token_type: 7,
    ..Default::default()
  };
  assert!(matches!(ContainerSpec::evaluate(None, &ctx), Ok(None)));
  assert!(matches!(
    ContainerSpec::evaluate(Some(&null), &ctx),
    Ok(None)
  ));
}

/// Shape errors identify the field but never echo a supplied value, which may
/// be a registry password or an expression resolving to one.
#[test]
fn rejects_invalid_shapes_without_echoing_credential_values() {
  let ctx = ExecutionContext::new_for_test();
  let secret = "registry-password-value";
  let token = mapping(vec![
    ("image", literal("alpine:3.20")),
    (
      "credentials",
      mapping(vec![
        ("username", literal("ci-user")),
        ("password", literal(secret)),
      ]),
    ),
    ("ports", literal("not-a-list")),
  ]);

  let result = ContainerSpec::evaluate(Some(&token), &ctx);
  let message = match result {
    Err(error) => error.to_string(),
    Ok(value) => format!("unexpected value: {value:?}"),
  };

  assert!(message.contains("ports"), "{message}");
  assert!(!message.contains(secret), "credential leaked: {message}");
}
