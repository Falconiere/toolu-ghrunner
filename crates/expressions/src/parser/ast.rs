//! AST types and core parser structure for GitHub Actions expressions.

use shared::RunnerError;

use super::super::lexer::Token;
use super::super::types::ExprValue;

/// AST node for a GitHub Actions expression.
#[derive(Debug, Clone)]
pub enum Expr {
  /// A literal value (string, number, bool, or null) parsed directly from a token.
  Literal(ExprValue),
  /// A reference to a top-level named context (`github`, `env`, `secrets`, etc.).
  Context {
    /// The context name, as written in the expression.
    name: String,
  },
  /// A `object.property` access.
  PropertyAccess {
    /// The expression being accessed.
    object: Box<Expr>,
    /// The property name being read off `object`.
    property: String,
  },
  /// An `object[index]` access.
  IndexAccess {
    /// The expression being indexed.
    object: Box<Expr>,
    /// The expression evaluated to produce the index.
    index: Box<Expr>,
  },
  /// An `object.*` wildcard access, collecting a property/element across all of `object`.
  WildcardAccess {
    /// The expression being wildcard-accessed.
    object: Box<Expr>,
  },
  /// A call to a built-in function.
  FunctionCall {
    /// The function name, as written in the expression.
    name: String,
    /// The evaluated argument expressions, in call order.
    args: Vec<Expr>,
  },
  /// A unary operator applied to an operand.
  UnaryOp {
    /// The unary operator.
    op: UnaryOperator,
    /// The expression the operator is applied to.
    operand: Box<Expr>,
  },
  /// A binary operator applied to two operands.
  BinaryOp {
    /// The binary operator.
    op: BinaryOperator,
    /// The left-hand operand.
    left: Box<Expr>,
    /// The right-hand operand.
    right: Box<Expr>,
  },
}

/// Binary operators in order of precedence (lowest to highest).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOperator {
  /// The `||` logical-or operator.
  Or,
  /// The `&&` logical-and operator.
  And,
  /// The `==` equality operator.
  Eq,
  /// The `!=` inequality operator.
  Neq,
  /// The `<` less-than operator.
  Lt,
  /// The `<=` less-than-or-equal operator.
  Le,
  /// The `>` greater-than operator.
  Gt,
  /// The `>=` greater-than-or-equal operator.
  Ge,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOperator {
  /// The `!` logical-not operator.
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
