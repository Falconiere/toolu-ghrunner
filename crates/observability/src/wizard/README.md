# wizard/

**What belongs here:** the PURE state machine behind `toolu-runner setup`
— the `WizardState` reducer over `StepEvent`s from the four stages
(authenticate → register → install → verify), keyboard mapping, terminal
command writers, rendering, and the pure verify-stage decision. No I/O, no
async, no network.

**What does NOT belong here:** actually running auth, registration,
service install, or the log-tail probe is the impure driver in the
`toolu-runner` bin (`setup_cmd.rs` for the render loop, `wizard_steps.rs`
for the async step executors) — this module only folds their outcomes
into `WizardState`. The equivalent state machine for the job-history TUI
is `observability::watch`, not here.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `input.rs` | `Action` / `action_for` | Maps a key event to a high-level `Action`; v1 is progress-display only, so the surface is just quit vs. ignore. |
| `state.rs` | `WizardState` | Pure reducer folding `StepEvent`s into the wizard model; `probe_skips` is the one read-only exception, inspecting on-disk state to pre-skip already-done stages. |
| `term.rs` | `enter_terminal` / `leave_terminal` | Testable alt-screen + cursor command writers over any `impl Write`; raw-mode enable/disable stays with the bin's terminal guard. |
| `ui.rs` | `render` | Ratatui rendering of the four-step checklist, active-stage detail, error line, and footer — a pure view over `state::WizardState`. |
| `verify.rs` | `verify_decision` | Pure decision for the final "is the runner online?" stage: service-active AND the log tail carries the online marker. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs`
declares `mod bar;` for `src/foo/bar.rs`) and import concrete paths.
