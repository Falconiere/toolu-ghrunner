//! AST types and core parser structure for GitHub Actions expressions.

use shared::RunnerError;

use super::super::lexer::Token;
use super::super::types::ExprValue;

/// AST node for a GitHub Actions expression.
#[derive(Debug, Clone)]
pub enum Expr {
  Literal(ExprValue),
  Context {
    name: String,
  },
  PropertyAccess {
    object: Box<Expr>,
    property: String,
  },
  IndexAccess {
    object: Box<Expr>,
    index: Box<Expr>,
  },
  WildcardAccess {
    object: Box<Expr>,
  },
  FunctionCall {
    name: String,
    args: Vec<Expr>,
  },
  UnaryOp {
    op: UnaryOperator,
    operand: Box<Expr>,
  },
  BinaryOp {
    op: BinaryOperator,
    left: Box<Expr>,
    right: Box<Expr>,
  },
}

/// Binary operators in order of precedence (lowest to highest).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOperator {
  Or,
  And,
  Eq,
  Neq,
  Lt,
  Le,
  Gt,
  Ge,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOperator {
  Not,
}

pub(super) struct Parser {
  pub(super) tokens: Vec<Token>,
  pub(super) pos: usize,
}

impl Parser {
  pub(super) fn peek(&self) -> Option<&Token> {
    self.tokens.get(self.pos)
  }

  pub(super) fn advance(&mut self) -> Option<&Token> {
    let tok = self.tokens.get(self.pos);
    if tok.is_some() {
      self.pos += 1;
    }
    tok
  }

  pub(super) fn expect_token(&mut self, expected: &Token) -> Result<(), RunnerError> {
    match self.peek() {
      Some(tok) if tok == expected => {
        self.advance();
        Ok(())
      },
      other => Err(RunnerError::Expression(format!(
        "expected {expected:?}, got {other:?}"
      ))),
    }
  }
}
