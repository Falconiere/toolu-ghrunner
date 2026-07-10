//! Per-job JSONL event journal: the local observability surface behind
//! `toolu-runner watch`. `writer` sinks the listener's `ListenerEvent`
//! stream to `<data_dir>/_diag/jobs/<ts>-<job_id>.jsonl`; `reader` replays
//! and tails those files; `types` pins the on-disk line contract (v1).

/// Incremental replay/tail reader + jobs-dir scanner.
pub mod reader;
/// On-disk line contract (v1) + conversions from `ListenerEvent`.
pub mod types;
/// Async sink task: listener channel → masked JSONL file, with retention.
pub mod writer;

pub use reader::{JobSummary, JournalReader, scan_jobs};
pub use types::{JOURNAL_VERSION, JournalEvent, JournalLine, sanitize_job_id};
pub use writer::{JOURNAL_RETAIN, PREACQ_BUF};
