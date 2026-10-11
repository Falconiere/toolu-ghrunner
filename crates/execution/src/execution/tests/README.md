# execution/tests/

**What belongs here:** flat test files for private execution-module behavior.

**What does not belong here:** production code or nested test directories.

| File | Purpose |
| --- | --- |
| `job_environment.rs` | Issue 69 captured-envelope replay for environment layers, step precedence, file commands, and job isolation. |
| `job_environment_actions.rs` | Issue 69 real Node stages and repeated nested composite environment probes. |
| `step_attrs.rs` | Evaluates captured timeout / continue-on-error tokens and pins the reference runner's diagnostics. |
| `step_display.rs` | Generates captured step display names at job start and main stage, including failure warnings and masking. |
| `step_timeout.rs` | Checks stalled action resolution against a parent deadline and cancellation. |
| `orphan_cleanup.rs` | Issue #89 sweep internals on real processes: kill of a tagged process, `exec env -i` between scan and kill (PID-reuse stand-in), unreaped zombie skip, expired deadline, `process.clean` parsing and id matching. |
| `job_cancellation.rs` | Real cleanup processes share one cancellation deadline and are reaped on expiry. |
| `composite_working_directory.rs` | Issue 81 captured-job replay through real Bash, per-step cwd, nested defaults and failure cleanup. |
| `docker_action_lifecycle.rs` | Real-Docker network and cleanup cases included by the opt-in `docker_action_linux_test` integration target. |
| `problem_matcher.rs` | Pinned setup-node definitions and captured real-tool diagnostics, owner lifecycle and bounds. |
| `problem_matcher_replay.rs` | Acquired-message replay through real Bash and nested composites, stderr, masking, paths and invalid command conclusions. |
| `problem_matcher_tsc.json`, `problem_matcher_eslint.json` | Pinned setup-node matcher definitions. |
| `problem_matcher_tsc.txt`, `problem_matcher_eslint.txt` | TypeScript 5.9.3 and ESLint 8.57.1 output captures. |
| `problem_matcher_provenance.md` | Fixture source revisions, capture commands and normalization. |
| `step_summary.rs` | Issue 83 captured-job replay with real shell/Node/composite summaries, masking, limits and cancellation. |
