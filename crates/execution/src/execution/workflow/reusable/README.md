# reusable/

**What belongs here:** reusable-workflow (`uses: owner/repo/path@ref` under
`on: workflow_call`) reference parsing, nesting-depth/circular-reference
guards, and input/output/secret resolution between a caller and a called
workflow.

**What does NOT belong here:** actually fetching or executing the referenced
workflow file — this module only resolves the reference and the
input/output/secret contract; fetching a remote workflow's YAML would reuse
`execution::actions::downloader`-style logic, not live here. General
workflow YAML parsing lives in the sibling `parser/` module.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `parse_ref.rs` | `parse_reusable_ref` | Parses a `owner/repo/path/to/workflow.yml@ref` reference string into a `ReusableWorkflowRef`. |
| `resolve.rs` | `resolve_reusable_invocation` | Checks nesting depth (`MAX_REUSABLE_WORKFLOW_DEPTH` = 4) and circular references, then validates/resolves the caller's inputs and secrets against the called workflow's definition. |
| `types.rs` | `ReusableWorkflowDef` | The `on: workflow_call` input/output/secret definitions, `SecretMode` (inherit vs explicit), and the validate/resolve helpers for inputs, secrets, and outputs. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs`
declares `mod bar;` for `src/foo/bar.rs`) and import concrete paths.
