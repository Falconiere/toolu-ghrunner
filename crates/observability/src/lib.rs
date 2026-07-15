//! Local observability surface: the per-job JSONL event [`journal`] and
//! the [`watch`] TUI that replays it. Depends only on `shared` and
//! `config` — never on the execution engine, listener, or `wire`.

/// Per-job JSONL event journal under `_diag/jobs/`; read by `watch`.
pub mod journal;
/// `watch` subcommand: TUI over the job journal (history + live tail).
pub mod watch;
/// Setup wizard: pure reducers + render helpers for guided first-run.
pub mod wizard;
