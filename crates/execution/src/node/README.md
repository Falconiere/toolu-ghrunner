# node/

**What belongs here:** Node.js runtime selection (node20/node24 migration,
host vs. container target), acquisition (preseed, cache, download) and
startup validation for `runs.using: node*` actions.

**What does NOT belong here:** dispatching a step to the downloaded Node
binary — that is `execution::handlers::node` / `node_exec`. Downloading and
resolving GitHub Actions themselves (as opposed to the Node runtime they run
under) lives in `execution::actions`.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `runtime.rs` | `ensure_node_runtime` | Pinned versions (upstream `externals.sh`: 20.20.2 / 24.21.0), per-target cache layout, the authoritative `TOOLU_RUNNER_NODE_DIR` preseed (upstream `externals/` naming), and the preseed → cache → download order. |
| `download.rs` | `install_runtime` | Streams a runtime tarball (nodejs.org or `actions/alpine_nodejs`), extracts into a staging dir, start-checks host-runnable runtimes, and commits with one rename. Retries transient transport failures (connection, 408/429/5xx, body cut mid-stream) up to 3 attempts. |
| `validate.rs` | `validate_runtime` | `<node> --version` must run and report the requested major. |
| `platform.rs` | `NodeTarget` | Host / job-container OS, CPU and libc (dynamic-loader and `/etc/*release` probes) and the unsupported-platform errors. |
| `migration.rs` | `select_node_version` | Upstream's node20 → node24 migration: job flags, `FORCE_JAVASCRIPT_ACTIONS_TO_NODE24` / `ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION`, step messages and the end-of-job warnings. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs`
declares `mod bar;` for `src/foo/bar.rs`) and import concrete paths.
