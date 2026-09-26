# Shell templates — Design

**Date:** 2026-09-26   **Status:** Approved   **Author:** Codex   **Topic:** #80 shell execution parity

## Problem

Unknown shells and custom templates silently execute bash. Host defaults gain
explicit bash pipefail semantics, python is rewritten to python3, and composites
have a second incomplete shell implementation.

## Non-Goals

1. Windows cmd/powershell, container hooks, or Docker action implementation.
2. Changes to expression evaluation, defaults precedence, cancellation, or reporting.
3. Claiming live parity from component tests or replay alone.

## Architecture

Add internal `handlers/shell_command.rs` to resolve a command, argument template,
extension and script fixup. Keep process lifecycle in `ScriptHandler`; delegate
host composite execution to it, as container composites already do. Resolve host
executables against the final step PATH and working directory, requiring an
executable file; container commands are resolved inside the container.

Follow actions/runner cab9d1c3901e45c7705889c4f88284fdd93f4ae5
`ScriptHandler.cs` and `ScriptHandlerHelpers.cs`. Host unspecified/empty shell
selects bash when executable on PATH, otherwise sh, always with `-e {0}`.
Container unspecified/empty shell selects sh with `-e {0}`. Explicit bash uses
`--noprofile --norc -e -o pipefail {0}`; sh uses `-e {0}`; python uses `{0}`;
pwsh uses `-command ". '{0}'"`. Do not rewrite python to python3. Unknown commands
(including python3) need an explicit template. Unknown shell without `{0}` fails.

Tokenize templates with existing shlex before substituting `{0}` into individual
arguments; this preserves a script path containing spaces as one argument and
does not introduce another shell or perform variable expansion. Quoted executable
paths and quoted arguments are supported. Invalid quoting, empty executable,
missing placeholder, NUL, or unsupported format fields fail clearly. Literal
braces use `{{`/`}}`; repeated `{0}` substitutes each occurrence. Known command
names use upstream extensions; custom executable names have no extension.
Pwsh scripts prepend `$ErrorActionPreference = 'stop'` and append
`if ((Test-Path -LiteralPath variable:\LASTEXITCODE)) { exit $LASTEXITCODE }`.
The built-in pwsh invocation escapes single quotes in its dot-sourced path.

## Interfaces / Schema

Internal `ShellCommand::resolve(shell: Option<&str>, env: &HashMap<String,String>,
working_dir: &Path, container: bool) -> Result<ShellCommand, RunnerError>`;
methods produce argv and write a tempfile in the host or mounted container temp
directory. Existing public ScriptParams and ShellScriptParams stay unchanged.
All configuration/spawn failures propagate as ScriptHandler errors naming the
invalid shell or executable; the existing step loop emits the failed result.

## Failure modes and edge cases

Absent and empty workflow-step shells mean default. Upstream action_yaml.json
requires shell on composite run steps; changing validation of malformed action
manifests is outside this issue. Valid composites keep their own explicit shell. Whitespace-only, malformed quotes/braces,
no `{0}`, unknown bare shell, missing executable and non-executable PATH entries
fail or fall through during default search; never silently execute another shell
for an explicit shell. When the caller omits PATH, use the runner PATH through execution::config; an
explicit empty PATH stays authoritative. Relative PATH entries resolve against working_dir. A default can fall back from unavailable
bash to sh, but never from an explicit missing interpreter. Temporary files stay
alive through execution and are removed on return. Container paths are translated
before substitution. Native nonzero exits and PowerShell errors fail the step.

## Acceptance criteria

- **AC-1:** Host default with/without bash and container default use upstream -e rules (80-S1); `false | true` succeeds by default and fails under explicit bash (80-S2).
- **AC-2:** Custom templates execute the requested real tool with preserved quoted arguments and space-containing paths; unknown bare shell, invalid template, and missing executable fail clearly (80-S3).
- **AC-3:** Python runs the python executable with a .py file; pwsh runs a .ps1 file with upstream fixups, and native nonzero exits/PowerShell errors fail (80-S4).
- **AC-4:** Captured production replay proves defaults.run, explicit overrides, composites, and container routing use the same contract (80-S5).
- **AC-5:** Full gate passes; user docs and coverage map identify evidence and unverified platform/backend/reference lanes.

## Acceptance evidence

| AC | Real input / observable / check |
| --- | --- |
| AC-1 | Real bash/sh processes under isolated executable PATH directories; default pipeline succeeds, explicit bash fails. `cargo test -p execution --test shell_templates_test`; Linux real-Docker replay default sh identity. |
| AC-2 | Real Perl plus custom bash templates, spaces in paths and arguments, repeated placeholder, missing executable, unknown bare interpreter and malformed templates. Same test command; errors must name cause and no marker script may run. |
| AC-3 | Real python and pwsh process probes check file extension and final conclusion. Same host tests; interpreter tests that require installation have explicit ignored opt-in commands, never count skipped tests as proof. |
| AC-4 | Reuse sanitized incoming_contexts_matrix_0.json and job_container_message.json provenance; retain wire tokens/UUIDs and label substitutions. `cargo test -p execution --test shell_templates_replay_test`; Linux container tests explicitly run with real Docker. Composite local manifests run real tools. |
| AC-5 | `./tools/check.sh all`; README.md and docs/test-coverage.md record actual commands/results and live-reference limitations. |

Compare applicable identical probes against pinned official runner when available.
Host contract applies to Linux/macOS and GitHub.com/GHES; job containers are Linux
only. GHES acquisition and a pinned reference runner are currently unavailable;
record as unverified, not passed. No backend-owned behavior changes here.

## Documentation impact

README.md shell guidance, docs/test-coverage.md AC/scenario map, handlers README
module row, and execution README composite description. Durable design and plan
live in tracked docs/specs and docs/plans, following existing epic artifacts;
docs/toolu is ignored.

## Open Questions

None. The brief authorizes autonomous decisions. Quoting preserves argv before
path substitution to satisfy the issue's explicit space-path contract; this is
not a claim to reproduce every .NET command-line parser quirk. Bare python3 is
not upstream built-in: use `python3 {0}`. Live unavailable lanes remain explicit.

## Spec review

Approved: all authored sections checked against #80 and pinned upstream. All five
scenario IDs map to observable process/replay evidence; unavailable live lanes
remain unverified. Jev omission assessment 0.2; direct checklist found no blocker.
