//! Ordered evaluation of acquired service-container declarations.

use std::collections::HashSet;

use expressions::types::ExprValue;
use shared::{RunnerError, TemplateToken};

use super::container_spec::{ContainerSpec, evaluate_token};
use crate::execution::context::ExecutionContext;

/// One evaluated service and its workflow DNS alias.
pub(crate) struct ServiceSpec {
  pub(crate) alias: String,
  pub(crate) container: ContainerSpec,
}

/// Evaluate service definitions before any Docker resource is created.
pub(crate) fn evaluate_services(
  token: Option<&TemplateToken>,
  ctx: &ExecutionContext,
) -> Result<Vec<ServiceSpec>, RunnerError> {
  let Some(token) = token else {
    return Ok(Vec::new());
  };
  let mut values = Vec::new();
  if token.token_type == 2 {
    for entry in token
      .d
      .as_deref()
      .ok_or_else(|| invalid("service mapping has no entries"))?
    {
      let ExprValue::String(alias) = evaluate_token(&entry.key, ctx)? else {
        return Err(invalid("service IDs must be strings"));
      };
      values.push((alias, evaluate_token(&entry.value, ctx)?));
    }
  } else {
    match evaluate_token(token, ctx)? {
      ExprValue::Null => return Ok(Vec::new()),
      ExprValue::Object(map) => {
        values.extend(map);
        values.sort_by(|left, right| left.0.cmp(&right.0));
      },
      ExprValue::Bool(_) | ExprValue::Number(_) | ExprValue::String(_) | ExprValue::Array(_) => {
        return Err(invalid("jobServiceContainers must be a mapping"));
      },
    }
  }
  let mut seen = HashSet::new();
  let mut services = Vec::new();
  for (alias, value) in values {
    if alias.is_empty()
      || !alias
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
    {
      return Err(invalid(
        "service ID must contain only letters, digits, underscores or hyphens",
      ));
    }
    if !seen.insert(alias.clone()) {
      return Err(invalid("duplicate service ID"));
    }
    let mut fields = match value {
      ExprValue::Null => continue,
      ExprValue::String(image) => {
        std::collections::HashMap::from([("image".to_owned(), ExprValue::String(image))])
      },
      ExprValue::Object(fields) => fields,
      ExprValue::Bool(_) | ExprValue::Number(_) | ExprValue::Array(_) => {
        return Err(invalid(
          "service must be an image string or container mapping",
        ));
      },
    };
    if let Some(ExprValue::String(image)) = fields.get_mut("image") {
      if let Some(stripped) = image.strip_prefix("docker://") {
        *image = stripped.to_owned();
      }
      if image.is_empty() {
        continue;
      }
    }
    let container = ContainerSpec::from_fields(fields)?;
    services.push(ServiceSpec { alias, container });
  }
  if !services.is_empty() && !cfg!(target_os = "linux") {
    return Err(invalid("service containers require a Linux runner host"));
  }
  Ok(services)
}

fn invalid(message: &str) -> RunnerError {
  RunnerError::Config(format!("invalid services: {message}"))
}
