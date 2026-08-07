# workflow/

**What belongs here:** workflow YAML parsing and the standalone
workflow-orchestration model — job dependency graph, matrix expansion,
trigger evaluation, and reusable-workflow resolution — as data types and
pure functions.

**What does NOT belong here:** actually running a job's steps — that is
`execution::job_runner` / `execution::steps_runner`, which consume
`workflow::types` (e.g. `WorkflowDefaults` via `execution::job_spec`) but own
the live execution path. `orchestrator::run_workflow`'s job execution is
simulated (always `Success`); the live path never calls it.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `job_graph.rs` | `topological_sort` | Topological ordering of jobs from a dependency map (cycle-detecting) and `ready_jobs` for the next batch whose dependencies are complete. |
| `matrix.rs` | `expand_matrix` | Expands a `strategy.matrix` config into all combinations: Cartesian product of base keys, then `exclude`, then `include`. |
| `orchestrator.rs` | `run_workflow` | Parses a workflow, evaluates its triggers, builds the job DAG, and (in this standalone model) simulates job execution job-by-job. |
| `parser.rs` | (mod decl) | Declares the `parser` sub-module (raw YAML → `WorkflowDefinition`); see its own README. |
| `reusable.rs` | (mod decl) | Declares the `reusable` sub-module (reusable-workflow ref parsing + resolution); see its own README. |
| `trigger.rs` | `evaluate_triggers` | Evaluates whether a workflow's `on:` triggers match an `EventPayload` (push/pull_request branch filters, simple glob matching). |
| `types.rs` | `WorkflowDefinition` | The parsed workflow data model: jobs, triggers, defaults, matrix/strategy config, and step definitions. |

## Sub-modules

- `parser/` — raw workflow YAML deserialization into `types::WorkflowDefinition`.
- `reusable/` — reusable-workflow (`uses: ./.github/workflows/x.yml`) ref
  parsing, circular/depth-limit checks, and input/output/secret resolution.

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs`
declares `mod bar;` for `src/foo/bar.rs`) and import concrete paths.
