# Expression engine parity — Design

**Date:** 2026-09-26   **Status:** Approved   **Author:** Codex   **Topic:** #79, part of #67

## Problem

The evaluator rejects valid numbers and silently mis-evaluates container equality,
unknown names, formatting and object filters. Workflow and composite evaluation
must share the corrected semantics. The oracle is actions/runner
`cab9d1c3901e45c7705889c4f88284fdd93f4ae5`, not an approximation of JavaScript.

## Non-Goals

1. Change workflow scheduling, context permission schemas, timeout handling (#99),
   or the composite scope and cleanup design (#102).
2. Claim Linux/macOS, GitHub.com/GHES or reference parity without recorded runs.
3. Change unrelated builtins or services except where required by these cases.

## Architecture

Retain the lexer/parser/evaluator pipeline. Store arrays and insertion-ordered
objects behind shared ownership so lookup clones retain identity; separately
parsed containers have distinct identity. Preserve dictionary entry order at
wire and JSON conversion boundaries. Migrate workspace constructors and explicit
mutation sites together, using copy-on-write when a context snapshot is amended.
This changes internal construction APIs but preserves thread-safe snapshots.

Validate named roots and function arity over the parsed tree before evaluating;
an undeclared root fails even in an unselected branch. Missing properties of a
declared root remain null. Caller-provided roots, including `vars`, are the
declarations; composite field restrictions remain in `composite_expr`.
Evaluate `case` predicates and selected results lazily: 3–255 arguments, odd
count, each evaluated predicate strictly boolean. Unselected runtime failures
remain unevaluated; syntax/name/arity errors remain validation failures.

Use one numeric parser for lexing and string coercion with upstream sign,
decimal/exponent, lowercase hex/octal prefix and signed 32-bit radix limits.
NaN is falsey, never equal, never ordered, and inequality negates equality.
Same-type strings use case-insensitive ordinal comparison; same-type containers
use reference identity. String search functions require primitives and use the
same ordinal casing; search/join skip unused second arguments. Mixed primitive types use numeric coercion. Format
numbers as invariant G15 including exponent thresholds, signs and nonfinite
spellings; JSON rendering follows upstream ToJson rather than assuming Rust's
default JSON serializer has the same number and nonfinite behavior.

Represent filtered wildcard results distinctly during evaluation so `.*` and
`[*]` project later accesses, omit absent members and flatten nested filters
without changing ordinary array indexing. Use the existing production paths:
`context_data`, `ExecutionContext::eval_context`, templates, step fields and
the shared composite evaluator introduced by #102.

## Interfaces / Schema

- `evaluate(&str, &EvalContext) -> Result<ExprValue, RunnerError>` stays stable.
- `ExprValue::Array(ExprArray)` wraps `Arc<Vec<ExprValue>>` plus a filtered flag;
  `ExprValue::Object(ExprObject)` wraps `Arc<IndexMap<String, ExprValue>>`.
  Constructors accept iterators; object copy-on-write mutations preserve snapshot
  isolation. Clone preserves identity; newly collected containers do not.
- Internal validation walks `Expr` using the current context's declared roots.
- Numeric parsing/string rendering are private expression helpers with sibling
  unit tests. No wire/config schema change or network dependency in expressions.

## Failure modes and edge cases

Invalid numeric tokens, unknown roots/functions, wrong arity, nonboolean selected
case predicates and malformed/out-of-range format placeholders return an error.
`false && fromJSON('bad')` and unselected case results do not evaluate runtime
errors. Empty/missing property remains null; empty wildcard yields an empty
filtered array. String lexing preserves UTF-8. Escaped format braces remain
literal; unmatched braces fail. Format evaluates only referenced arguments after
validating the template, caches repeated references, accepts `{0:}`, rejects
nonempty specifiers and indexes outside the byte range. toJSON renders bare
nonfinite spellings as upstream does; these are not valid JSON roundtrip inputs. Zero, negative zero, subnormal, rounding carry,
G15 exponent boundaries, infinities, NaN and radix overflow are explicit vectors.
Read-only shared snapshots are safe across threads; amendments detach storage.

## Acceptance criteria

- **AC-1:** Pinned-reference numeric and case vectors return the same typed
  results/errors, including strict predicate types and unselected errors.
- **AC-2:** Pinned-reference truth tables distinguish same/distinct containers,
  exact numbers, mixed primitives, case-insensitive strings and NaN; every NaN
  equality/order result is false and NaN inequality is true.
- **AC-3:** Declared missing properties produce null, undeclared roots produce
  clear errors, and logical/case short-circuiting preserves selected values.
- **AC-4:** JSON key insertion order, pretty output, roundtrip, G15 extremes,
  format braces/index errors and both wildcard syntaxes match the reference.
- **AC-5:** Sanitized captured-message replay executes these semantics in real
  script/env/with/if and supported composite fields, with exact shell outputs
  and visible failures. Standalone tests live in `crates/expressions/src/tests/`.
- **AC-6:** The full unsuppressed repository gate passes and documentation maps
  every original criterion and 79-S1–S5 to exact evidence and applicability.

## Acceptance evidence

| AC / scenario | Input and exact result/boundary | Runnable proof |
| --- | --- | --- |
| AC-1 / 79-S1 | `-1`, `+1`, `0xff`, `NaN`, `Infinity`; `case(false, fromJSON('bad'), true, 42, 0)` → number 42; even arity/nonboolean selected predicate → error | `cargo test -p expressions`; committed reference vectors with source revision and generator provenance |
| AC-2 / 79-S2–S3 | `github == github` → true; separately parsed equal JSON containers → false; `NaN != NaN` → true; all NaN orders false; `'A' < 'b'` true, `null == false` true | Same crate test command; explicit pair/operator truth table from pinned reference |
| AC-3 / 79-S3 | `github.absent` → null; unknown root → error, including unselected branch; `false && fromJSON('bad')` → false | Same crate command plus captured-message failure replay |
| AC-4 / 79-S4 | JSON `{"z":1,"a":2}` retains z then a with two-space pretty layout; `format('{2}', 'a')` errors; escaped braces literal; filters project real captured array/object values; G15 threshold and rounding vectors | Same crate command and pinned-reference corpus comparison |
| AC-5 / 79-S5 | Existing `incoming_contexts_matrix_0.json` captured #68 message, with documented expression/action substitutions preserving token types/UUIDs and assigning the authored `parity` action ID; real Bash writes exact expected values and composite output, bad root visibly fails | `cargo test -p execution --test expression_parity_test`; identical committed action/workflow revision on official and toolu runners |
| AC-6 / all | Gate and coverage matrix name fixtures, expected outputs, exact tests and evidence revision/links | `./tools/check.sh all`; review README and `docs/test-coverage.md` mapping |

Pure semantics apply on Linux/macOS and both GitHub.com/GHES because the evaluator
is shared. Production replay proves message wiring with real local tools, not
server acceptance. Record platform and backend lanes separately. Required live
reference comparison must use the same workflow/action revision and equivalent
hosts; absent runtime/credentials/infrastructure remains unverified and blocks
claiming ready. No UI/service result is newly owned by this change.

## Documentation impact

Update README expression support and `docs/test-coverage.md` with #79's complete
matrix, provenance, exact observed results and unverified lanes. Keep this durable
spec in tracked `docs/specs/`, matching other epic children; execution ledger
lives in `docs/toolu/plans/` under the installed workflow.

## Open Questions

None in the implementation contract. Reference runtime availability is being
checked; unavailable required infrastructure must be reported to the orchestrator
rather than replaced with fabricated passing evidence. Shared ordered containers
were selected from the observed deep-clone/HashMap behavior (Jev agreed with 0.98
confidence); exact semantics remain pinned-source and test decisions.

## Spec review

Approved after source review of Case, ExpressionConstants, ExpressionUtility,
EvaluationResult, ToJson, Format and Index at the pinned SHA. Review corrected
format laziness/empty specifiers and nonfinite JSON behavior before approval.
Jev assessed evidence sufficiency at 0.8; the source checks resolved the remaining
contract details. No unresolved implementation findings.
