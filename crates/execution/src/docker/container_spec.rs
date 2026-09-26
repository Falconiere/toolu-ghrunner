//! Evaluation and validation of a job's `container:` declaration.

use std::collections::HashMap;

use expressions::types::ExprValue;
use shared::{RunnerError, TemplateToken};

use super::container_options::parse_container_options;
use crate::execution::context::ExecutionContext;

/// Fully evaluated job-container declaration ready for owned lifecycle setup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerSpec {
  /// Docker image reference.
  pub image: String,
  /// Optional registry credentials used when pulling the image.
  pub credentials: Option<ContainerCredentials>,
  /// Environment values supplied to the job container.
  pub env: HashMap<String, String>,
  /// Ports declared by the workflow for service discovery.
  pub ports: Vec<String>,
  /// User-supplied volume declarations; lifecycle validates mount syntax.
  pub volumes: Vec<String>,
  /// Validated Docker create options as argv entries.
  pub options: Vec<String>,
}

/// Registry credentials attached to a job-container image declaration.
#[derive(Clone, PartialEq, Eq)]
pub struct ContainerCredentials {
  /// Registry account name.
  pub username: String,
  /// Registry account password or access token.
  pub password: String,
}

impl std::fmt::Debug for ContainerCredentials {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    formatter
      .debug_struct("ContainerCredentials")
      .field("username", &"***")
      .field("password", &"***")
      .finish()
  }
}

impl ContainerSpec {
  /// Evaluate an optional wire declaration using the job expression context.
  ///
  /// A missing or null declaration is returned as `None`. A string declaration
  /// is an image shorthand; object declarations require a nonempty `image`
  /// string and can supply the remaining supported fields.
  ///
  /// # Errors
  ///
  /// Returns [`RunnerError::Config`] when the declaration has an invalid shape
  /// or unsupported values, and intentionally omits supplied values from its
  /// diagnostics so credentials cannot leak through an error path.
  pub fn evaluate(
    token: Option<&TemplateToken>,
    ctx: &ExecutionContext,
  ) -> Result<Option<Self>, RunnerError> {
    let Some(token) = token else {
      return Ok(None);
    };
    let value = evaluate_token(token, ctx)?;
    match value {
      ExprValue::Null => Ok(None),
      ExprValue::String(image) => Self::from_image(image).map(Some),
      ExprValue::Object(fields) => Self::from_fields(fields.into_iter().collect()).map(Some),
      other @ (ExprValue::Bool(_) | ExprValue::Number(_) | ExprValue::Array(_)) => {
        Err(spec_error(&format!(
          "container must be a string or object, got {}",
          other.type_name()
        )))
      },
    }
  }

  fn from_image(image: String) -> Result<Self, RunnerError> {
    if image.trim().is_empty() {
      return Err(spec_error("container image must not be empty"));
    }
    Ok(Self {
      image,
      credentials: None,
      env: HashMap::new(),
      ports: Vec::new(),
      volumes: Vec::new(),
      options: Vec::new(),
    })
  }

  pub(super) fn from_fields(mut fields: HashMap<String, ExprValue>) -> Result<Self, RunnerError> {
    for key in fields.keys() {
      if !matches!(
        key.as_str(),
        "image" | "credentials" | "env" | "ports" | "volumes" | "options"
      ) {
        return Err(spec_error(&format!("unsupported container field `{key}`")));
      }
    }
    let image = take_string(&mut fields, "image", true)?;
    if image.trim().is_empty() {
      return Err(spec_error("container image must not be empty"));
    }
    let credentials = take_credentials(&mut fields)?;
    let env = take_string_map(&mut fields, "env")?;
    let ports = take_string_list(&mut fields, "ports")?;
    let volumes = take_string_list(&mut fields, "volumes")?;
    let options = match fields.remove("options") {
      None | Some(ExprValue::Null) => Vec::new(),
      Some(ExprValue::String(value)) => parse_container_options(&value)?,
      Some(value) => return Err(field_type_error("options", "string", &value)),
    };
    Ok(Self {
      image,
      credentials,
      env,
      ports,
      volumes,
      options,
    })
  }
}

pub(super) fn evaluate_token(
  token: &TemplateToken,
  ctx: &ExecutionContext,
) -> Result<ExprValue, RunnerError> {
  match token.token_type {
    0 => ctx
      .interpolate_string(token.lit.as_deref().unwrap_or_default())
      .map(ExprValue::String)
      .map_err(|_error| spec_error("container expression evaluation failed")),
    1 => token
      .seq
      .as_deref()
      .unwrap_or_default()
      .iter()
      .map(|entry| evaluate_token(entry, ctx))
      .collect::<Result<Vec<_>, _>>()
      .map(ExprValue::array),
    2 => {
      let mut fields = HashMap::new();
      for entry in token.d.as_deref().unwrap_or_default() {
        let key = evaluate_token(&entry.key, ctx)?;
        let ExprValue::String(key) = key else {
          return Err(spec_error("container object keys must be strings"));
        };
        fields.insert(key, evaluate_token(&entry.value, ctx)?);
      }
      Ok(ExprValue::object(fields))
    },
    3 => ctx
      .evaluate_expression(token.expr.as_deref().unwrap_or_default())
      .map_err(|_error| spec_error("container expression evaluation failed")),
    5 => Ok(ExprValue::Bool(token.bool_val.unwrap_or_default())),
    6 => Ok(ExprValue::Number(token.num_val.unwrap_or_default())),
    7 => Ok(ExprValue::Null),
    other => Err(spec_error(&format!(
      "unsupported container token type {other}"
    ))),
  }
}

fn take_string(
  fields: &mut HashMap<String, ExprValue>,
  name: &str,
  required: bool,
) -> Result<String, RunnerError> {
  match fields.remove(name) {
    Some(ExprValue::String(value)) => Ok(value),
    None if required => Err(spec_error(&format!("container field `{name}` is required"))),
    None | Some(ExprValue::Null) => Ok(String::new()),
    Some(value) => Err(field_type_error(name, "string", &value)),
  }
}

fn take_credentials(
  fields: &mut HashMap<String, ExprValue>,
) -> Result<Option<ContainerCredentials>, RunnerError> {
  let Some(value) = fields.remove("credentials") else {
    return Ok(None);
  };
  if matches!(value, ExprValue::Null) {
    return Ok(None);
  }
  let ExprValue::Object(credentials) = value else {
    return Err(field_type_error("credentials", "object", &value));
  };
  for key in credentials.keys() {
    if !matches!(key.as_str(), "username" | "password") {
      return Err(spec_error(&format!(
        "unsupported container credentials field `{key}`"
      )));
    }
  }
  let mut credentials = credentials.into_iter().collect();
  let username = take_string(&mut credentials, "username", true)?;
  let password = take_string(&mut credentials, "password", true)?;
  Ok(Some(ContainerCredentials { username, password }))
}

fn take_string_map(
  fields: &mut HashMap<String, ExprValue>,
  name: &str,
) -> Result<HashMap<String, String>, RunnerError> {
  let Some(value) = fields.remove(name) else {
    return Ok(HashMap::new());
  };
  if matches!(value, ExprValue::Null) {
    return Ok(HashMap::new());
  }
  let ExprValue::Object(entries) = value else {
    return Err(field_type_error(name, "object", &value));
  };
  let mut resolved = HashMap::with_capacity(entries.len());
  for (key, value) in entries {
    let ExprValue::String(value) = value else {
      return Err(spec_error(&format!(
        "container field `{name}.{key}` must be a string"
      )));
    };
    resolved.insert(key, value);
  }
  Ok(resolved)
}

fn take_string_list(
  fields: &mut HashMap<String, ExprValue>,
  name: &str,
) -> Result<Vec<String>, RunnerError> {
  let Some(value) = fields.remove(name) else {
    return Ok(Vec::new());
  };
  if matches!(value, ExprValue::Null) {
    return Ok(Vec::new());
  }
  let ExprValue::Array(entries) = value else {
    return Err(field_type_error(name, "array", &value));
  };
  let mut resolved = Vec::with_capacity(entries.len());
  for value in entries {
    let ExprValue::String(value) = value else {
      return Err(spec_error(&format!(
        "container field `{name}` entries must be strings"
      )));
    };
    resolved.push(value);
  }
  Ok(resolved)
}

fn field_type_error(field: &str, expected: &str, value: &ExprValue) -> RunnerError {
  spec_error(&format!(
    "container field `{field}` must be a {expected}, got {}",
    value.type_name()
  ))
}

fn spec_error(message: &str) -> RunnerError {
  RunnerError::Config(format!("invalid job container: {message}"))
}

#[cfg(test)]
#[path = "tests/container_spec.rs"]
mod tests;
