# watch/

**What belongs here:** the `toolu-runner watch` ratatui TUI — the pure
job-list/step-tree/log-ring reducer, the keyboard-to-action mapping
(including the cancel-confirm modal), and the pure rendering over that
model. The impure event loop, multi-dir job discovery, and the SIGINT
cancel-delivery live in the sibling `watch.rs` that declares this folder.

**What does NOT belong here:** reading or writing journal files is
`observability::journal` (`JournalReader`, `scan_jobs`); this module only
consumes what that module produces. The setup wizard's TUI (a different
screen, driven by the bin) is `observability::wizard`, not here.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `input.rs` | `Action` / `action_for` | Maps a key event to a high-level `Action`, honoring the cancel-confirm modal that swallows every key until answered. |
| `state.rs` | `App` | Pure reducer: turns journal lines / job summaries into the job list, step tree, and bounded 10k-line log ring the UI renders. |
| `ui.rs` | `render` | Ratatui rendering — header, job list, step tree, log pane, footer — as a pure view over `state::App`. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs`
declares `mod bar;` for `src/foo/bar.rs`) and import concrete paths.
