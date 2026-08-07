# handlers/

**What belongs here:** the concrete `runs.using` step handlers (script,
node, node_exec, docker, composite) and the dispatch decision
(`resolve::resolve_handler`) that picks among them — plugin first, then the
four built-ins.

**What does NOT belong here:** the actual end-to-end action download/run
orchestration — that is `execution::action_exec` /
`execution::node_stage`, which call into these handlers for the low-level
process-spawn work. Composite step iteration lives in
`execution::composite_exec`, not `handlers::composite` (which today only
holds the depth-tracked scope/output-evaluation scaffolding).

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `composite.rs` | `prepare_composite` | Enters the depth tracker for a composite step and builds its output-evaluation scope (`CompositeOutputs`), plus a simple `steps.X.outputs.Y` output-reference resolver. |
| `docker.rs` | `parse_docker_uses` | Parses a `docker://image:tag` `uses:` string into `(image, tag)`, defaulting the tag to `latest`. |
| `node.rs` | `build_action_env` | Node.js action env building: `determine_script` picks the pre/main/post entrypoint, `input_env_key` does the `INPUT_*` name transform, and this assembles `GITHUB_ACTION_PATH`/`INPUT_*`/`STATE_*`. |
| `node_exec.rs` | `execute_node_action` | Spawns `node {script}` for one stage, streams stdout/stderr live, and waits bounded by timeout/cancellation via `step_timeout::wait_bounded`. |
| `resolve.rs` | `resolve_handler` | Picks which handler runs a step: plugin registry first (by `runs.using` name), then the built-in script/node/docker/composite match. |
| `script.rs` | `ScriptHandler` | Spawns a `run:` step's shell script, streams stdout/stderr live, and waits bounded by timeout/cancellation; also hosts the `stream_output`/`forward_lines`/`bounded_drain` helpers shared with `node_exec`. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs`
declares `mod bar;` for `src/foo/bar.rs`) and import concrete paths.
