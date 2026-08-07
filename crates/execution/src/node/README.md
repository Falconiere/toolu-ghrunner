# node/

**What belongs here:** Node.js runtime version resolution, download, and
on-disk caching (`data_dir/node/{version}`) for `runs.using: node*` actions.

**What does NOT belong here:** dispatching a step to the downloaded Node
binary — that is `execution::handlers::node` / `node_exec`. Downloading and
resolving GitHub Actions themselves (as opposed to the Node runtime they run
under) lives in `execution::actions`.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `runtime.rs` | `ensure_node_runtime` | Resolves a Node major version to a pinned release, downloads and extracts the tarball into the cache dir if missing, and returns the cached binary path. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs`
declares `mod bar;` for `src/foo/bar.rs`) and import concrete paths.
