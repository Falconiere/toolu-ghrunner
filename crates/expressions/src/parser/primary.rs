//! Primary expression parsing (literals, contexts, function calls).

use shared::RunnerError;

use super::super::lexer::Token;
use super::super::types::ExprValue;
use super::ast::{Expr, Parser};

impl Parser {
  pub(super) fn parse_primary(&mut self) -> Result<Expr, RunnerError> {
    match self.peek().cloned() {
      Some(Token::StringLit(s)) => {
        self.advance();
        Ok(Expr::Literal(ExprValue::String(s)))
      },
      Some(Token::NumberLit(n)) => {
        self.advance();
        Ok(Expr::Literal(ExprValue::Number(n)))
      },
      Some(Token::BoolLit(b)) => {
        self.advance();
        Ok(Expr::Literal(ExprValue::Bool(b)))
      },
      Some(Token::Null) => {
        self.advance();
        Ok(Expr::Literal(ExprValue::Null))
      },
      Some(Token::LParen) => {
        self.advance();
        let inner = self.parse_or()?;
        self.expect_token(&Token::RParen)?;
        Ok(inner)
      },
      Some(Token::Ident(name)) => {
        self.advance();
        self.parse_ident_expr(name)
      },
      other => Err(RunnerError::Expression(format!(
        "unexpected token: {other:?}"
      ))),
    }
  }

  fn parse_ident_expr(&mut self, name: String) -> Result<Expr, RunnerError> {
    // Function call: ident(args)
    if matches!(self.peek(), Some(Token::LParen)) {
      self.advance();
      let args = self.parse_args()?;
      self.expect_token(&Token::RParen)?;
      return Ok(Expr::FunctionCall { name, args });
    }

    Ok(Expr::Context { name })
  }

  fn parse_args(&mut self) -> Result<Vec<Expr>, RunnerError> {
    let mut args = Vec::new();
    if matches!(self.peek(), Some(Token::RParen)) {
      return Ok(args);
    }
    args.push(self.parse_or()?);
    while matches!(self.peek(), Some(Token::Comma)) {
      self.advance();
      args.push(self.parse_or()?);
    }
    Ok(args)
  }
}
