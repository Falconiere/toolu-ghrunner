# Composite expression, scope, and failure semantics — Design

**Date:** 2026-09-25   **Status:** Approved   **Author:** Codex   **Topic:** Issue #102, composite action execution parity

## Problem

Composite `run`, `env`, `with`, and output strings use a four-prefix regex renderer that silently erases valid expressions. The inner loop returns at the first uncontinued failure, so later `failure()` and `always()` cleanup never runs. Nested action results, posts, environment, and state are not consistently scoped. These behaviors can produce a successful looking but semantically wrong job.

## Non-Goals

1. Rework top-level workflow expression parsing, Docker actions, or GHES protocol transport.
2. Implement #81's composite working-directory precedence and path resolution; this issue must consume that implementation when it lands and test the field's expression behavior.
3. Claim Linux, GHES, service UI, or pinned-reference parity from a local macOS replay. Missing lanes remain explicitly unverified.

## Architecture

Use `expressions::evaluator` and `expressions::template` for all composite fields. A composite invocation enters its own `ExecutionContext` scope with its resolved action inputs, step results, and local action status. Each inner step evaluates against one snapshot made at step start. Validate expression roots and status functions against the pinned [action metadata schema](https://github.com/actions/runner/blob/cab9d1c3901e45c7705889c4f88284fdd93f4ae5/src/Runner.Worker/action_yaml.json) before evaluation:

| Composite field | Permitted roots | Extra functions |
| --- | --- | --- |
| `run`, `env`, `with`, `shell`, `working-directory`, `name`, `continue-on-error` | `github`, `inputs`, `strategy`, `matrix`, `steps`, `job`, `runner`, `env`; `github.action_path` only in step `env` | `hashFiles` |
| `if` | Same roots | `always`, `failure`, `cancelled`, `success`, `hashFiles` |
| mapped output value | `github`, `strategy`, `matrix`, `steps`, `inputs`, `job`, `runner`, `env` | None beyond ordinary expression functions |
| input default | `github`, `strategy`, `matrix`, `job`, `runner` | `hashFiles` |

Invalid syntax or prohibited context is a hard evaluation failure; a missing property in an allowed root keeps the evaluator's null-to-empty string semantics. `secrets`, `needs`, and `vars` are not permitted by this pinned composite schema; callers pass secrets as explicit action inputs. `github.action_path` is available in composite step `env`, not `run`, as documented by GitHub.
The ordinary pure functions (`contains`, `startsWith`, `endsWith`, `format`, `join`, `toJSON`, `fromJSON`) remain available in every expression field; the table lists functions with field-specific permission.

The composite loop records each raw outcome and effective conclusion separately. A failed step with `continue-on-error: true` has `failure` outcome and `success` conclusion; it does not change the aggregate composite result. A non-continued failure sets the aggregate to failure and the next ordinary step's implicit `success()` becomes false. The loop still evaluates later `failure()` and `always()` steps. A malformed `if` expression is reported as failure and ends that composite loop, matching the pinned upstream handler. Cancellation sets cancelled status and permits only conditions eligible under cancellation within the remaining parent bound.

File commands update the job's live env/PATH for later inner and outer steps; a step-level `env` override lasts only for that step. Each invocation's `steps` result map and `STATE_*` map use its full invocation path, so repeated names cannot collide. Nested Node `post` registrations reach the job's LIFO queue, retaining the originating scope and own report identity. The enclosing top-level step ID owns inner log and annotation lines. The parent timeout/cancel bound encompasses the full composite, including nested scripts and actions; processes are killed and reaped when that bound fires.

The choice to extend `ExecutionContext` scopes instead of maintaining a separate composite results map lets Node stages, posts, and expression rendering read one authoritative state. It requires explicit scope restoration on every success/error/cancel path.

Pinned reference: [actions/runner CompositeActionHandler at cab9d1c](https://github.com/actions/runner/blob/cab9d1c3901e45c7705889c4f88284fdd93f4ae5/src/Runner.Worker/Handlers/CompositeActionHandler.cs). Context restrictions: [GitHub Actions contexts reference](https://docs.github.com/en/actions/reference/workflows-and-actions/contexts).

## Interfaces / Schema

- `ExecutionContext` owns per-invocation action inputs and local status alongside the existing scoped `steps` and action state. Scope entry/restore use the current invocation path; output rendering occurs before restoring the parent.
- Composite field rendering accepts `(&ExecutionContext, &EvalContext, CompositeField, &str) -> Result<String, RunnerError>`. `CompositeField` selects the table's root/function policy, including the special `github.action_path` field rule.
- Nested action dispatch returns both `ActionOutcome` and any registered posts to the owning job queue; one inner action failure cannot discard already registered posts.
- `CompositeResult` retains final conclusion, declared outputs, and job-scoped file-command effects. It never exports undeclared inner outputs.
- No wire message or public configuration schema changes.

## Failure modes and edge cases

- Empty/absent condition means implicit `success()`; an explicitly `always()` or `failure()` condition can run after failure. Invalid `if` fails the composite with a parent-scoped error; malformed render expressions fail their step and leave later eligible cleanup runnable unless the error is in `if`.
- An absent allowed property stringifies to empty; syntax errors, unknown functions, and forbidden contexts do not silently become empty.
- A nested `uses` resolution error records that inner step's failed outcome, attributes the error to the outer log, and evaluates subsequent conditions. `continue-on-error` changes the effective conclusion only.
- Failed or skipped inner steps retain `steps.<id>.outcome` and `.conclusion`; repeated invocations with the same IDs do not reuse old values. Parent inputs, `steps`, and `github.action_path` are restored, including after error/cancel.
- `GITHUB_ENV`/`GITHUB_PATH` persist through later steps and the parent job. `GITHUB_OUTPUT` remains in the current invocation until declared output mapping exports it. `GITHUB_STATE` remains private to one action instance and its post.
- Cancellation or timeout kills and reaps a running child, prevents ordinary later work, and respects eligible cleanup within the shared bound. Final cancelled/failed result takes precedence over a later successful cleanup.

## Acceptance criteria

- **AC-1:** Given a real composite with functions, operators, and bracket access over supported `github`, `env`, `inputs`, `steps`, and runner contexts, executed `run`/`env`/`with`/`if`/`shell`/working-directory values and mapped outputs have the fixture's exact expected strings; malformed syntax or a forbidden context fails visibly instead of producing an empty string.
- **AC-2:** Given exit 1, malformed render expressions, and hard nested-action errors followed by ordinary, `failure()`, and `always()` steps, the exact marker sequence and raw/effective results match the pinned runner with `continue-on-error` both false and true; the outer result retains an uncontinued failure.
- **AC-3:** Given two repeated nested invocations with identical input and step names but different values, their outputs, outcomes, conclusions, action state, and parent contexts remain isolated and restored.
- **AC-4:** Given inner `GITHUB_ENV`, `GITHUB_PATH`, `GITHUB_OUTPUT`, and `GITHUB_STATE` writes plus a Node action with pre/main/post, later inner and outer steps see only the upstream-scoped effects and posts run in LIFO order under the correct action instance.
- **AC-5:** Given an inner cwd change, timeout, or cancellation, the whole composite follows the parent bound, leaves no live child, retains parent log/annotation attribution, and reports `Failure` for timeout or `Cancelled` for cancellation after eligible cleanup, as on the pinned runner on equivalent hosts.

## Acceptance evidence

| AC / issue scenario | Real input and exact observable result | Runnable proof and applicability |
| --- | --- | --- |
| AC-1 / S1 | Committed `action.yml` and workflow pass `who=world`; `format('hello {0}', inputs['who'])` writes `hello world` in the real script, step env and nested `with`. `contains(github['repository'], 'toolu-ghrunner') && steps.first.outputs.answer == '42'` runs the guarded step, whose output maps to `hello world/42`. Expression-valued shell selects `bash`; expression-valued cwd selects the fixture's `subdir`. A malformed function and forbidden `secrets.*` produce a parent `##[error]`, not an empty value. | Production `Runner::execute_job` replay of sanitized #68 acquisition with the checked-in action plus real Bash; `cargo test -p execution --test composite_semantics_test`. Same action/workflow revision on pinned reference and toolu macOS runner; #81's cwd implementation must be present for its assertion. |
| AC-2 / S2 and S4 | Failure/no-continue markers: `fail`, `failure-cleanup`, `always-cleanup`; `ordinary` absent; outer failure. Continue markers: `fail`, `ordinary`, `always-cleanup`; `steps.fail.outcome=failure`, `.conclusion=success`; outer success. Nested missing action and malformed `run` expression each emit parent `##[error]` followed by eligible cleanup; malformed `if` terminates the inner loop with failure. | Same production replay and real local actions; `cargo test -p execution --test composite_semantics_test`. Reference workflow comparison on equivalent hosts. |
| AC-3 / S3 | Two parent calls pass `one`/`two`; each nested child writes exactly `one/one` or `two/two` from its own input and same-named child result, exports that declared output, and parent `inputs.who`/`steps.first` retain their original values after both calls. | Sanitized #68 captured message replay plus committed nested actions; `cargo test -p execution --test composite_scope_test`. Reference workflow comparison. |
| AC-4 / S5 | `GITHUB_ENV` sets `COMPOSITE_VALUE=inner` and `GITHUB_PATH` prepends a fixture `bin` for later inner and outer shells. A step-only `LOCAL_ONLY=one` is absent in the next step. `GITHUB_OUTPUT` yields only declared `answer=42`. Two Node actions save `STATE_who=one/two`; their posts log `two` then `one`, each with its own report ID. | Captured-message replay with real shell and Node binary, no passing skip; `cargo test -p execution --test composite_file_commands_test`. Reference workflow comparison. |
| AC-5 / S4-S5 | Parent ID carries every inner log and error annotation; fixture `subdir` is the observed shell cwd. A child that writes `started`, then sleeps beyond its bound never writes `late`; timeout ends `Failure`, observed-start cancellation ends `Cancelled`, later ordinary work is absent, and eligible cleanup runs within the remaining bound. The child PID is reaped. | Production replay with real subprocess and captured step, `cargo test -p execution --test composite_bounds_test`; #81 cwd test reused after merge. Equivalent macOS and Linux reference/toolu runs. Real GitHub.com job log/timeline and annotation IDs must corroborate the local parent-ID check. |

Captured payloads retain UUID/contextName differences and token types; action path substitutions are labelled transformations. `docs/test-coverage.md` records fixture revision/hash, exact test names/results, and links for every completed live lane. GitHub.com UI is checked from real acquired jobs; GHES is applicable to supported versions but remains unverified without a server and credentials. Linux and macOS results are recorded separately. A missing reference runner, Node, or live backend cannot count as passing evidence. The full gate is `./tools/check.sh all`.

## Documentation impact

Update `README.md` composite support wording, `docs/architecture.md` scope/control-flow description, and `docs/test-coverage.md` with the AC/S1-S5 evidence matrix and explicit unverified lanes.

## Open Questions

None. Issue #81 owns cwd precedence; this issue integrates its expression site and records the dependency without changing #81's path algorithm. The brief authorizes resolving design choices from the issue, pinned upstream, and code without an interactive approval round.
