# actions/

**What belongs here:** resolving a `uses:` reference (local vs remote,
subpath, cache key), downloading and extracting a remote action's tarball
into the on-disk cache, and parsing `action.yml`/`action.yaml` into a typed
manifest.

**What does NOT belong here:** actually running an action once resolved —
that is `execution::action_exec` (top-level dispatch) and
`execution::node_stage` / `execution::handlers::node` /
`execution::handlers::node_exec` for the Node.js entrypoints. Composite
`uses:` recursion lives in `execution::composite_uses`, not here.

## Contents

| File | Primary item | Purpose |
| --- | --- | --- |
| `downloader.rs` | `download_and_extract_action` | Downloads an action tarball (watermark-cached), extracts it with GitHub's-prefix stripping and a tar-slip guard, and marks the cache complete. |
| `manifest.rs` | `parse_action_manifest` | Parses `action.yml`/`action.yaml` YAML into `ActionDefinition` (inputs, outputs, `runs` — `RunsUsing::Node`/`Composite`/`Docker`, composite `steps:`). |
| `prefetch.rs` | `ActionFetcher` | Single-flight download cache keyed by resolved action ref, shared by job-start prefetch (`spawn_prefetch`, deduped via `resolver::resolve_action_refs`) and step-time `action_exec::resolve_remote_action`; a failed fetch evicts its entry for a fresh retry. |
| `resolver.rs` | `parse_action_ref` | Parses a `uses:` string into an `ActionRef` (remote `owner/repo[/subpath]@ref` or local `./path`), with cache-key/tarball-URL builders and traversal guards. |

When you add a file here, add its row above so the index stays current. No
`mod.rs` barrel — declare submodules from the parent file (`src/foo.rs`
declares `mod bar;` for `src/foo/bar.rs`) and import concrete paths.
