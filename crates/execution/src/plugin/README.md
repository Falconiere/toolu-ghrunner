# plugin/

**What belongs here:** the compiled-in plugin trait and registry that let a
custom step type hook into the job lifecycle ahead of the built-in handler
dispatch.

**What does NOT belong here:** the built-in handler dispatch itself (plugin →
script → node → docker → composite) — that decision and the concrete
handlers live in `execution::handlers`. Step-level execution context lives in
`execution::context`, not here.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `registry.rs` | `PluginRegistry` | Holds compiled-in plugins in insertion order, with by-name lookup, replace-on-duplicate registration, and iteration. |
| `trait_def.rs` | `RunnerPlugin` | Trait a plugin implements: `name()` for dispatch matching, `on_job_init` / `execute_step` / `on_job_cleanup` lifecycle hooks. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs`
declares `mod bar;` for `src/foo/bar.rs`) and import concrete paths.
