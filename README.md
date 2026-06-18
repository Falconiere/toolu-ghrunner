# toolu-runner

Standalone self-hosted GitHub Actions runner written in Rust. Runs real jobs
against github.com and GHES. Extracted from the yamless-runner engine.

> **Status: pre-alpha.** The plan and spec are at `docs/toolu/` (gitignored)
> and the scaffolding is being laid down step by step. See
> `docs/toolu/plans/2026-06-18-toolu-runner-standalone.md` for the build plan.

## Quick start (post-v1)

```bash
# Register a runner against a GitHub repo
toolu-runner register --url https://github.com/owner/repo --token <reg_token> \
  --name my-runner --labels self-hosted,linux,x64

# Run the listener
toolu-runner run

# Check status
toolu-runner status
```

## Development

Requires Rust 1.94.1 (pinned in `rust-toolchain.toml`).

```sh
# Build
cargo build --workspace

# Test
cargo nextest run --workspace

# Lint
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check

# Pre-commit
lefthook run pre-commit
```

## Layout

- `crates/toolu-runner-shared` — cross-cutting types, startup init.
- `crates/toolu-runner-protocol` — sync, no-I/O, no-network protocol layer.
- `crates/toolu-runner` — listener, engine, reporting, CLI binary.
- `tools/check.sh` — local code-quality gate.
- `.github/workflows/` — CI matrix.
- `install.sh` — install script (mirrors `actions/runner` UX).

## License

MIT — see `LICENSE`.
