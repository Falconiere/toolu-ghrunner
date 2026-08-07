# parser/

**What belongs here:** deserializing workflow YAML into the raw serde shapes
and converting those into `workflow::types::WorkflowDefinition` — jobs,
steps, matrix, and trigger parsing.

**What does NOT belong here:** the resulting data model itself
(`workflow::types`) or anything that consumes it (job graph, matrix
expansion, orchestration) — those live in the parent `workflow/` module.
Action-manifest (`action.yml`) parsing is a separate concern in
`execution::actions::manifest`, not here.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `jobs.rs` | `parse_jobs` | Converts raw job/step/matrix/strategy YAML shapes into `JobDefinition`/`StepDefinition`/`MatrixConfig`. |
| `parse.rs` | `parse_workflow` | Top-level entry point: deserializes the YAML into `RawWorkflow`, then delegates to `triggers`/`jobs` to build the full `WorkflowDefinition`. |
| `raw_types.rs` | `RawWorkflow` | The `serde`-deserializable raw shapes (`RawWorkflow`/`RawJob`/`RawStrategy`/`RawStep`) mirroring workflow YAML's kebab-case keys. |
| `triggers.rs` | `parse_trigger` | Parses the `on:` key (string, sequence, or mapping form) into a `TriggerConfig`, including push/pull_request branch/tag/path filters. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs`
declares `mod bar;` for `src/foo/bar.rs`) and import concrete paths.
