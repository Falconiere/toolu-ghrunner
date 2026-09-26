# Step summaries — Design

**Date:** 2026-09-26   **Status:** Approved   **Author:** Codex   **Topic:** #83

## Problem

The engine truncates and discards `GITHUB_STEP_SUMMARY`. GitHub job summaries
remain empty, and deleting the summary also prevents other file-command reads.

## Non-Goals

1. Implement Docker actions (#75), legacy Checks attachment APIs, or a fake
   summary backend for offline mode.
2. Claim GHES live parity. The orchestrator explicitly authorized this lane as
   **unverified/skipped — no secrets available** on 2026-09-26.

## Architecture

Keep HTTP out of execution. A new summary collector reads each completed
handler's file independently of ENV/OUTPUT/STATE/PATH. Capture immutable text
before file reuse. Mask using the job's existing runtime masker before emitting
`RunnerEvent::StepSummary`. The listener masks again, queues uploads serially,
and drains before reporting job completion. This preserves ordering and isolates
one failed upload from the next. Journal only the summary ID and byte count,
never its body. No summary bodies or signed URLs enter diagnostics.

Shell and job-container shell steps collect after stdout dispatch, including
failure/cancellation. Node pre/main/post each collect under their report identity.
Embedded composite children get distinct UUIDs and separate files; nested Node
children also get distinct summary UUIDs, with errors attributed to the visible
parent. No expression-name strings are sent as backend IDs.

## Interfaces / Schema

- `execution::step_summary::collect(path, report_id, summary_id, ctx, events)`
  emits a masked summary or an error annotation; it does not mutate conclusions.
- `RunnerEvent::StepSummary { step_id: String, content: String }` carries one
  complete masked document. Journal stores `{step_id, size}` only.
- Receiver RPCs at `twirp/results.services.receiver.Receiver/`:
  `GetStepSummarySignedBlobURL` with `workflow_run_backend_id`,
  `workflow_job_run_backend_id`, `step_backend_id`; response has `summary_url`,
  `blob_storage_type`, `soft_size_limit`. Accept protobuf integer number/string.
- PUT the UTF-8 body as Azure BlockBlob when requested. Respect the server's
  soft limit after masking/newline normalization. `CreateStepSummaryMetadata`
  sends the same IDs, `size`, and RFC3339 `uploaded_at`; require `ok: true`.
- Wire module owns transport and safe errors. Listener owns a bounded serial
  queue, one attempt per document with bounded HTTP timeouts, and drain.
  Only blob-level retries follow the pinned upstream transport policy.
  Queue closure does not abort completed documents on job cancellation.

## Failure modes and edge cases

Pinned oracle: actions/runner `cab9d1c3901e45c7705889c4f88284fdd93f4ae5`,
`FileCommandManager.cs`, `Constants.cs`, `ResultsHttpClient.cs`.
Missing/empty/deleted files upload nothing. Raw byte size 1,048,576 is accepted;
1,048,577 is rejected, never truncated, using the upstream
`$GITHUB_STEP_SUMMARY upload aborted, supports content up to a size of 1024k, got
1024k. For more information see: https://docs.github.com/actions/using-workflows/workflow-commands-for-github-actions#adding-a-markdown-summary`
error annotation. Integer KiB values match upstream. Annotation alone does not
change shell/step/job outcome. Read errors annotate without dropping other file
commands. Normalize CR/LF to host newlines, append a newline per logical line,
and use UTF-8 replacement decoding/BOM stripping like StreamReader.

Check metadata and cap reads to limit+1 to bound memory even if the file grows.
Missing Results endpoint skips upload. Malformed replies, soft-limit rejection,
HTTP errors and exhausted retries report safe diagnostics and leave conclusions
unchanged. Never log response bodies, SAS query strings, or summary text. Do not replay the complete three-stage upload after failure; move to the next
item, matching upstream Results queue handling.
Previously completed summaries survive later failed or cancelled steps.

## Acceptance criteria

- **AC-1:** Real multiple-step shell/Node/composite probes write/append/overwrite
  distinct summaries; absent and deleted files produce none; earlier summaries
  remain after a later failure (83-S1/S4).
- **AC-2:** Real UTF-8 files at the byte limit upload intact; one byte over emits
  the upstream error annotation, no summary, and preserves upstream conclusions
  (83-S2).
- **AC-3:** Registered and dynamic mask probes reach upload as masked text only;
  diagnostics/journal do not retain raw summary content (83-S3).
- **AC-4:** Real Results Service metadata/blob/UI verification and cancellation
  preserve completed summaries; one failed upload does not lose later documents.
  Exercise container jobs on Linux and host execution on Linux/macOS (83-S4).
- **AC-5:** Full `./tools/check.sh all` passes, docs and coverage map describe
  actual behavior and evidence; GHES remains explicitly unverified/skipped.

## Acceptance evidence

AC-1–3: sibling `execution/tests/step_summary.rs` replays the committed sanitized
`incoming_contexts_matrix_0.json` acquisition with labelled shell/Node/composite
probe substitutions. Assert event content/order, annotations and conclusions;
real shell writes limit-sized UTF-8 files. `cargo test -p execution step_summary`.
AC-3–4: wire/listener sibling tests exercise production upload construction and
controlled transport failures without fabricated success; inspect journal byte
metadata. `cargo test -p wire step_summary`; `cargo test -p listener step_summary`.
AC-4: `.github/workflows/step-summary-83.yml` runs committed probes on Linux/macOS
and equivalent toolu/pinned-reference hosts. Record real GitHub.com run/job/UI
links and observed conclusions, including oversize, failure and cancellation.
Container job lane uses Linux Docker; macOS container jobs are not supported.
AC-5: full gate and CI on both platforms; update `docs/test-coverage.md` and
`docs/step-summaries.md`. Missing/failed lanes are not passing evidence.

## Documentation impact

Add `docs/step-summaries.md`; update the coverage map, architecture/event guidance,
and README rows for added modules. Keep this durable design under `docs/specs`
because `docs/toolu` is ignored. Local execution ledger stays in `docs/toolu/plans`.

## Open Questions

No blocking design questions. GHES lane explicitly skipped by orchestrator;
GitHub.com service and pinned-reference observations remain required execution
work, with actual results recorded rather than inferred from component tests.
