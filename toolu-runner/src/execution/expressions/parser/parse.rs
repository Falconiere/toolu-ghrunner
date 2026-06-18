//! Expression parsing entry point.

use super::super::lexer::lex;
use super::ast::{Expr, Parser};
use shared::RunnerError;

/// Parse a GitHub Actions expression string into an AST.
///
/// # Errors
///
/// Returns `RunnerError::Expression` on syntax errors.
pub fn parse(input: &str) -> Result<Expr, RunnerError> {
  let tokens = lex(input)?;
  let mut p = Parser { tokens, pos: 0 };
  let expr = p.parse_or()?;
  if p.pos < p.tokens.len() {
    return Err(RunnerError::Expression(format!(
      "unexpected token at position {}",
      p.pos
    )));
  }
  Ok(expr)
}
