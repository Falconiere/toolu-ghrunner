//! Precedence climbing and postfix parsing for expressions.

use shared::RunnerError;

use super::ast::{BinaryOperator, Expr, Parser, UnaryOperator};
use super::super::lexer::Token;

// ── Precedence climbing ─────────────────────────────────────────────

impl Parser {
  pub(super) fn parse_or(&mut self) -> Result<Expr, RunnerError> {
    let mut left = self.parse_and()?;
    while matches!(self.peek(), Some(Token::Or)) {
      self.advance();
      let right = self.parse_and()?;
      left = Expr::BinaryOp {
        op: BinaryOperator::Or,
        left: Box::new(left),
        right: Box::new(right),
      };
    }
    Ok(left)
  }

  fn parse_and(&mut self) -> Result<Expr, RunnerError> {
    let mut left = self.parse_equality()?;
    while matches!(self.peek(), Some(Token::And)) {
      self.advance();
      let right = self.parse_equality()?;
      left = Expr::BinaryOp {
        op: BinaryOperator::And,
        left: Box::new(left),
        right: Box::new(right),
      };
    }
    Ok(left)
  }

  fn parse_equality(&mut self) -> Result<Expr, RunnerError> {
    let mut left = self.parse_comparison()?;
    while let Some(op) = self.peek_equality_op() {
      self.advance();
      let right = self.parse_comparison()?;
      left = Expr::BinaryOp {
        op,
        left: Box::new(left),
        right: Box::new(right),
      };
    }
    Ok(left)
  }

  fn parse_comparison(&mut self) -> Result<Expr, RunnerError> {
    let mut left = self.parse_unary()?;
    while let Some(op) = self.peek_comparison_op() {
      self.advance();
      let right = self.parse_unary()?;
      left = Expr::BinaryOp {
        op,
        left: Box::new(left),
        right: Box::new(right),
      };
    }
    Ok(left)
  }

  fn parse_unary(&mut self) -> Result<Expr, RunnerError> {
    if matches!(self.peek(), Some(Token::Not)) {
      self.advance();
      let operand = self.parse_unary()?;
      return Ok(Expr::UnaryOp {
        op: UnaryOperator::Not,
        operand: Box::new(operand),
      });
    }
    self.parse_postfix()
  }
}

// ── Postfix (property/index/call) ───────────────────────────────────

impl Parser {
  pub(super) fn parse_postfix(&mut self) -> Result<Expr, RunnerError> {
    let mut expr = self.parse_primary()?;

    loop {
      match self.peek() {
        Some(Token::Dot) => {
          self.advance();
          expr = self.parse_dot_access(expr)?;
        },
        Some(Token::LBracket) => {
          self.advance();
          let index = self.parse_or()?;
          self.expect_token(&Token::RBracket)?;
          expr = Expr::IndexAccess {
            object: Box::new(expr),
            index: Box::new(index),
          };
        },
        _ => break,
      }
    }

    Ok(expr)
  }

  fn parse_dot_access(&mut self, object: Expr) -> Result<Expr, RunnerError> {
    match self.peek() {
      Some(Token::Star) => {
        self.advance();
        Ok(Expr::WildcardAccess {
          object: Box::new(object),
        })
      },
      Some(Token::Ident(_)) => {
        let name = match self.advance() {
          Some(Token::Ident(n)) => n.clone(),
          _ => return Err(RunnerError::Expression("expected identifier".to_owned())),
        };
        Ok(Expr::PropertyAccess {
          object: Box::new(object),
          property: name,
        })
      },
      other => Err(RunnerError::Expression(format!(
        "expected property name or *, got {other:?}"
      ))),
    }
  }
}

// ── Operator helpers ────────────────────────────────────────────────

impl Parser {
  fn peek_equality_op(&self) -> Option<BinaryOperator> {
    match self.peek() {
      Some(Token::Eq) => Some(BinaryOperator::Eq),
      Some(Token::Neq) => Some(BinaryOperator::Neq),
      _ => None,
    }
  }

  fn peek_comparison_op(&self) -> Option<BinaryOperator> {
    match self.peek() {
      Some(Token::Lt) => Some(BinaryOperator::Lt),
      Some(Token::Le) => Some(BinaryOperator::Le),
      Some(Token::Gt) => Some(BinaryOperator::Gt),
      Some(Token::Ge) => Some(BinaryOperator::Ge),
      _ => None,
    }
  }
}
