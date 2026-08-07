# functions/

**What belongs here:** the built-in GitHub Actions expression functions
(`contains`, `startsWith`, `format`, `toJSON`, `hashFiles`, etc.) called by
the evaluator, plus their private helpers.

**What does NOT belong here:** the AST and parsing that produce a
`FunctionCall` node live in `../parser/`; the tree-walking evaluator that
dispatches into `call_function` lives in `../evaluator.rs`; the `${{ }}`
runtime value type these functions operate on is `../types.rs`.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `builtins.rs` | `call_function` | Case-insensitive dispatch of a built-in function name to its implementation (`success`, `contains`, `format`, `join`, etc.). |
| `glob_walk.rs` | `literal_prefix`, `search_roots`, `walk` | Directory traversal for `hashFiles()`, reproducing `@actions/glob`'s depth-first, byte-order-sorted `globGenerator` walk. |
| `hash.rs` | `hash_files` | GitHub-compatible `hashFiles()`: folds per-file SHA-256 digests (raw bytes) into one outer SHA-256, hex-encoded, in `glob_walk` traversal order. |
| `json_convert.rs` | `fn_to_json`, `fn_from_json` | `toJSON` / `fromJSON` implementations and the `ExprValue` <-> `serde_json::Value` converters backing them. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs` declares
`mod bar;` for `src/foo/bar.rs`) and import concrete paths.
