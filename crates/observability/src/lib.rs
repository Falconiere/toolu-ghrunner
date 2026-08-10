//! Local observability surface: the per-job JSONL event [`journal`].
//! Depends only on `shared` — never on the execution engine, listener,
//! or `wire`.

/// Per-job JSONL event journal under `_diag/jobs/`.
pub mod journal;
