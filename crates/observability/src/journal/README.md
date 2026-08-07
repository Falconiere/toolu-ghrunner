# journal/

**What belongs here:** the per-job JSONL event journal — the on-disk line
contract, the async sink that drains the listener's `ListenerEvent`
channel into masked `_diag/jobs/<ts>-<job_id>.jsonl` files with retention,
and the incremental reader/scanner that replays and tails those files.

**What does NOT belong here:** rendering or interacting with journal data
is `observability::watch` (the TUI); this module only produces and reads
the files. The pure `WizardState` reducer that drives the setup wizard
lives in `observability::wizard`, not here. Secret masking logic itself
is `shared::SecretMasker` — `writer` only calls it per line.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `reader.rs` | `JournalReader` / `scan_jobs` | Incremental replay-then-tail reader over one journal file, plus a jobs-dir scanner that summarizes every `.jsonl` file for the job list. |
| `types.rs` | `JournalLine` / `JournalEvent` | The on-disk v1 line contract: a version/seq/timestamp envelope wrapping a flattened, internally-tagged event enum, decoupled from `shared::events`. |
| `writer.rs` | `spawn` | Async sink task: masks and appends one JSON line per `ListenerEvent` to the job's journal file, buffering pre-acquire events and pruning to the newest 50; never fails the job. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs`
declares `mod bar;` for `src/foo/bar.rs`) and import concrete paths.
