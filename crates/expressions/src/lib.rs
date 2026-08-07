//! The GitHub Actions `${{ }}` expression evaluator: lexer, parser, evaluator, and templating.

/// Context objects (`github`, `env`, `secrets`, `steps`, `matrix`, etc.) fed to expression evaluation.
pub mod context_data;
/// Evaluates a parsed expression AST against an [`EvalContext`](evaluator::EvalContext).
pub mod evaluator;
/// Built-in expression functions (`contains`, `format`, `hashFiles`, etc.) and their dispatch.
pub mod functions;
/// Tokenizes a `${{ }}` expression string into a stream of tokens.
pub mod lexer;
/// Parses a token stream into an expression AST.
pub mod parser;
/// Expands `${{ }}` expressions embedded in template strings (e.g. workflow YAML values).
pub mod template;
/// The dynamic value type (`ExprValue`) expressions evaluate to.
pub mod types;
