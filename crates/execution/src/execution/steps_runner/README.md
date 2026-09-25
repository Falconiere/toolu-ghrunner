# steps_runner/

**What belongs here:** focused helpers for the step runner that form cohesive
sub-responsibilities and would otherwise make `steps_runner.rs` exceed the
repository file-size limit.

**What does NOT belong here:** the main step loop or per-step orchestration;
those stay in `steps_runner.rs`.

| File | Primary item | Purpose |
| --- | --- | --- |
| `script_support.rs` | script-step support functions | Builds script-step environment and file-command paths, dispatches stdout workflow commands, and merges step outputs. |
