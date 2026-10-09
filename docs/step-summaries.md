# Step summaries

Write Markdown to `$GITHUB_STEP_SUMMARY` in a shell, Node action, or embedded
composite step. Each completed handler contributes a separate document to the
GitHub job summary. Appending and overwriting follow ordinary file semantics;
empty or deleted files (and files holding only a UTF-8 BOM) contribute nothing.
Completed documents survive later step failure or cancellation. Node pre/main/post
stages capture their own files. Docker container actions are not collected yet:
their `$GITHUB_STEP_SUMMARY` file is created but never read.

The runner reads only the regular file it created. A symlink or any other file
type at that path is rejected with an error annotation, because job and action
containers can replace the file and the host must not upload what a link points at.
It also rejects files larger than 1,048,576 raw bytes with an error annotation.
It never truncates them and the annotation alone does not fail the step or job.
The accepted content uses replacement decoding for invalid UTF-8, strips a UTF-8
BOM, and normalizes line endings. Registered secrets and `::add-mask::` values
are masked before the document enters the event stream, and again at upload.

The listener uploads documents serially through the Results Service signed-URL,
blob, and metadata APIs, and drains the queue before reporting job completion.
Blob PUTs retry transient failures up to three times with 30-second request
timeouts; the complete upload transaction is never replayed. Individual upload
failures produce safe diagnostics without changing the job's
conclusion or preventing later documents from being attempted. Bodies and signed
URLs are excluded from summary diagnostics; the journal records only IDs and
masked byte sizes. A missing Results Service endpoint skips uploads.

## Verification and applicability

The `execution::step_summary_tests` tests replay the sanitized GitHub acquisition
`crates/execution/tests/incoming_contexts_matrix_0.json` with labelled real
shell and committed Node/composite probes. Run:

```sh
cargo test -p execution --lib step_summary
cargo test -p wire -p listener -p observability step_summary
./tools/check.sh all
```

`.github/workflows/step-summary-83.yml` defines equivalent toolu and pinned
reference lanes for live GitHub.com summary verification on Linux and macOS,
plus Linux container execution. Enable only on prepared runner labels; the
reference must use actions/runner commit
`cab9d1c3901e45c7705889c4f88284fdd93f4ae5`. Record actual run and job-summary
observations before claiming live parity. Local collector tests and controlled
HTTP failures do not establish successful GitHub service/UI integration.

GHES is explicitly **unverified/skipped** by the epic orchestrator. macOS
container jobs are not applicable. Live GitHub.com evidence is pending; no
successful upload or UI observation is implied by the workflow's presence.
