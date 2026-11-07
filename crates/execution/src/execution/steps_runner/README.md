# steps_runner/

**What belongs here:** focused helpers for the step runner that form cohesive
sub-responsibilities and would otherwise make `steps_runner.rs` exceed the
repository file-size limit.

**What does NOT belong here:** the main step loop or per-step orchestration;
those stay in `steps_runner.rs`.

| File | Primary item | Purpose |
| --- | --- | --- |
| `pre_phase.rs` | `run_pre_phase` | Upstream's pre-job phase: every top-level repository action's `pre` as a numbered row after the setup rows and before the first main step; its worst conclusion joins the job's. |
| `script_support.rs` | script-step support functions | Builds script-step environment and file-command paths, dispatches stdout workflow commands, and merges step outputs. |
| `step_reports.rs` | `StepName` / `start_step` | Step lifecycle events (start with any display-name warning, skip, pre-handler failure, completion). |
