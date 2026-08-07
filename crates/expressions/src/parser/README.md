# parser/

**What belongs here:** the GitHub Actions expression parser — the `Expr` AST,
operator enums, the `Parser` struct, precedence-climbing binary/postfix
parsing, and primary-expression (literal/context/function-call) parsing.

**What does NOT belong here:** tokenizing the raw `${{ }}` string into
`Token`s happens in `../lexer.rs`, which this parser consumes; walking the
resulting `Expr` tree to a value happens in `../evaluator.rs`; the built-in
functions a `FunctionCall` node dispatches to live in `../functions/`.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `ast.rs` | `Expr`, `BinaryOperator`, `UnaryOperator`, `Parser` | AST node types, operator enums, and the core `Parser` struct (token buffer + cursor) the rest of this module operates on. |
| `parse.rs` | `parse` | Entry point: lexes the input, drives `parse_or`, and errors on trailing unconsumed tokens. |
| `precedence.rs` | `Parser::parse_or` and the precedence-climbing chain | Binary-operator precedence climbing (`or` → `and` → equality → relational → …) and postfix parsing. |
| `primary.rs` | `Parser::parse_primary` | Primary expression parsing: literals, context names, property/index access, and function calls. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs` declares
`mod bar;` for `src/foo/bar.rs`) and import concrete paths.
