//! Upstream's step display-name helpers for expression tokens.
//!
//! A step `name:` like `Bad ${{ x }}` reaches the runner as the expression
//! `format('Bad {0}', x)`. Until it can be evaluated, upstream shows the
//! prettified `BasicExpressionToken.ToDisplayString` text
//! (`Bad ${{ x }}`), and it only evaluates once every named value the
//! expression references is present (`CheckHasRequiredContext`).

use shared::RunnerError;

use crate::functions::format;
use crate::parser::{BinaryOperator, Expr, UnaryOperator, parse};
use crate::types::ExprValue;

#[cfg(test)]
#[path = "tests/display.rs"]
mod tests;

/// The context and function names an expression references, as written.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct References {
  /// Top-level named values (`steps`, `matrix`, ...).
  pub contexts: Vec<String>,
  /// Called function names.
  pub functions: Vec<String>,
}

/// Collect every named value and function an expression references.
///
/// # Errors
/// Returns the parser's error for an expression that does not parse.
pub fn references(expression: &str) -> Result<References, RunnerError> {
  let mut found = References::default();
  collect(&parse(expression)?, &mut found);
  Ok(found)
}

/// Upstream's `TrimDisplayString`: drop leading whitespace, keep the first line.
pub fn first_line(text: &str) -> &str {
  let trimmed = text.trim_start_matches([' ', '\t', '\r', '\n']);
  trimmed.split(['\r', '\n']).next().unwrap_or(trimmed)
}

/// Upstream's `BasicExpressionToken.ToDisplayString`: a `format()` whose
/// first argument is a string literal is unrolled back into template text,
/// each remaining argument shown as `${{ argument }}`; anything else (or a
/// format that cannot render) shows as `${{ expression }}`.
pub fn display_string(expression: &str) -> String {
  unrolled_format(expression)
    .unwrap_or_else(|| first_line(&format!("${{{{ {expression} }}}}")).to_owned())
}

fn unrolled_format(expression: &str) -> Option<String> {
  let Ok(Expr::FunctionCall { name, args }) = parse(expression) else {
    return None;
  };
  let (Some(Expr::Literal(ExprValue::String(template))), Some(parameters)) =
    (args.first(), args.get(1..))
  else {
    return None;
  };
  if !name.eq_ignore_ascii_case("format") || parameters.is_empty() {
    return None;
  }
  let shown: Vec<String> = parameters.iter().map(format_parameter).collect();
  let rendered = format::render(template, shown.len(), |index| {
    Ok(ExprValue::String(
      shown.get(index).cloned().unwrap_or_default(),
    ))
  })
  .ok()?
  .coerce_to_string();
  (!rendered.is_empty()).then(|| first_line(&rendered).to_owned())
}

/// Upstream's `ConvertFormatParameterToExpression`: a single parenthesized
/// group such as `(a || b)` loses its outer parentheses.
fn format_parameter(node: &Expr) -> String {
  let text = to_expression(node);
  let group = text
    .strip_prefix('(')
    .and_then(|rest| rest.strip_suffix(')'))
    .filter(|inner| !inner.is_empty() && !inner.contains(['(', ')']));
  format!("${{{{ {} }}}}", group.unwrap_or(&text))
}

/// Upstream's `ExpressionNode.ConvertToExpression`.
fn to_expression(node: &Expr) -> String {
  match node {
    Expr::Literal(value) => literal(value),
    Expr::Context { name } => name.clone(),
    Expr::PropertyAccess { object, property } => format!("{}.{property}", to_expression(object)),
    Expr::IndexAccess { object, index } => match index.as_ref() {
      Expr::Literal(ExprValue::String(key)) if is_keyword(key) => {
        format!("{}.{key}", to_expression(object))
      },
      Expr::Literal(_)
      | Expr::Context { .. }
      | Expr::PropertyAccess { .. }
      | Expr::IndexAccess { .. }
      | Expr::WildcardAccess { .. }
      | Expr::FunctionCall { .. }
      | Expr::UnaryOp { .. }
      | Expr::BinaryOp { .. } => format!("{}[{}]", to_expression(object), to_expression(index)),
    },
    Expr::WildcardAccess { object } => format!("{}.*", to_expression(object)),
    Expr::FunctionCall { name, args } => {
      let args: Vec<String> = args.iter().map(to_expression).collect();
      format!("{name}({})", args.join(", "))
    },
    Expr::UnaryOp {
      op: UnaryOperator::Not,
      operand,
    } => format!("!{}", to_expression(operand)),
    Expr::BinaryOp { op, left, right } => format!(
      "({} {} {})",
      to_expression(left),
      operator(*op),
      to_expression(right)
    ),
  }
}

fn literal(value: &ExprValue) -> String {
  match value {
    ExprValue::String(text) => format!("'{}'", text.replace('\'', "''")),
    ExprValue::Null => "null".to_owned(),
    ExprValue::Bool(_) | ExprValue::Number(_) | ExprValue::Array(_) | ExprValue::Object(_) => {
      value.coerce_to_string()
    },
  }
}

fn operator(op: BinaryOperator) -> &'static str {
  match op {
    BinaryOperator::Or => "||",
    BinaryOperator::And => "&&",
    BinaryOperator::Eq => "==",
    BinaryOperator::Neq => "!=",
    BinaryOperator::Lt => "<",
    BinaryOperator::Le => "<=",
    BinaryOperator::Gt => ">",
    BinaryOperator::Ge => ">=",
  }
}

/// Upstream's `ExpressionUtility.IsLegalKeyword`.
fn is_keyword(text: &str) -> bool {
  let mut chars = text.chars();
  chars
    .next()
    .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
    && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn collect(node: &Expr, found: &mut References) {
  match node {
    Expr::Literal(_) => {},
    Expr::Context { name } => found.contexts.push(name.clone()),
    Expr::PropertyAccess { object, .. } | Expr::WildcardAccess { object } => collect(object, found),
    Expr::IndexAccess { object, index } => {
      collect(object, found);
      collect(index, found);
    },
    Expr::FunctionCall { name, args } => {
      found.functions.push(name.clone());
      for arg in args {
        collect(arg, found);
      }
    },
    Expr::UnaryOp { operand, .. } => collect(operand, found),
    Expr::BinaryOp { left, right, .. } => {
      collect(left, found);
      collect(right, found);
    },
  }
}
