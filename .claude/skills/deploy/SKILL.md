---
name: deploy
description: Build every toolu-runner release artifact on this Mac and push it with scripts/deploy.sh — dev (images tagged -dev) or prod (images, GitHub release, Homebrew). Run only when the user explicitly asks for a deploy.
argument-hint: --env dev|prod --tag vX.Y.Z [--change-log]
disable-model-invocation: true
---

# deploy

Drive `scripts/deploy.sh` end to end for the user. The script does the work;
your job is the checks before it, the confirmation, the long-running watch,
and the report. Arguments given: `$ARGUMENTS`

What each env pushes (from the script header — `scripts/deploy.sh --help`):

| env | pushes | never touches |
| --- | --- | --- |
| `dev` | `ghcr.io/falconiere/toolu-ghrunner:<v>-dev`, `:<v>-dev-docker`, `ghcr.io/falconiere/toolu-ghrunner-macos:<v>-dev` | GitHub release, Homebrew, `:latest` |
| `prod` | the same images at `:<v>`, then the GitHub release (prerelease when the tag has `-`), then the Homebrew formula (stable tags) | `:latest` / `:vN` |

`<v>` is the tag minus its leading `v`.

## Rules

- **Never deploy prod on your own judgment.** The user must have named
  `--env prod` in this conversation AND said yes to the dry-run plan in step 4.
  Earlier approvals (another tag, another session) do not count.
- **Never bypass a failed check.** No `--no-verify`, no editing the script to
  skip the gate, no deleting a release or tag to "retry clean". Stop and report.
- **The script's confirm prompt reads `/dev/tty`, which you do not have.** For
  prod, pass `--yes` only after the user's explicit yes in chat (step 4).
- One deploy at a time; never run two in parallel (they share `dist/`).

## Steps

### 1. Pin the inputs

- `env`: `dev` or `prod`, from `$ARGUMENTS` or the user. Missing → ask.
- `tag`:
  - prod: must be `v` + `[workspace.package] version` from `Cargo.toml`.
    Check with `bash scripts/assert-version.sh <tag>` (exit 1 = mismatch →
    stop; bumping the version is the release-pr flow, not this skill).
  - dev: any `[A-Za-z0-9][A-Za-z0-9._-]*` tag; default to `v<Cargo version>`
    and say so. Note it overwrites any earlier `<v>-dev` image.
- `--change-log` (prod only): release notes from git-cliff over the commits
  since the last tag, instead of the `CHANGELOG.md` `## [<v>]` section. If
  `bash scripts/changelog-extract.sh <v>` fails (no section) and the user did
  not ask for `--change-log`, ask which they want — do not pick for them.

### 2. Check the host (each line must pass; report the first failure)

```bash
uname -sm                                   # Darwin arm64 — anything else: stop, cannot build darwin
docker buildx version && docker info >/dev/null   # Docker Desktop running
docker login ghcr.io </dev/null             # already logged in? (needs write:packages)
rustup --version && cargo --version
gh auth status                              # prod only
command -v git-cliff                        # --change-log only
```

Prod also needs a clean tree on `origin/main`:

```bash
git status --porcelain                      # must be empty
git fetch -q origin main && git merge-base --is-ancestor HEAD origin/main
gh release view <tag> -R Falconiere/toolu-ghrunner   # must FAIL (no release yet)
```

A missing login is the user's to fix: suggest `! docker login ghcr.io` /
`! gh auth login` (the `!` runs it in their session); never handle their token.

### 3. Dry run

```bash
scripts/deploy.sh --env <env> --tag <tag> [--change-log] --dry-run
```

It prints the plan and every command, runs none. Exit 0 required; exit 2 is a
bad argument (message says which), exit 1 a prod version mismatch.

### 4. Confirm

Show the user, in a short list: env, tag, the three image refs, and for prod
the release + Homebrew lines and the notes source (the dry run's `notes:`
line). For prod, also warn: creating the release creates the git tag, which
may start `.github/workflows/release.yml` (`push: tags: v*`) — CI would
re-push the images (incl. `:latest`) and fail its own publish step.

- dev: proceed unless the user objects.
- prod: wait for an explicit yes. Anything else → stop.

### 5. Run (long: gate + 4 builds + 3 image pushes, often 30–60 min)

```bash
scripts/deploy.sh --env <env> --tag <tag> [--change-log] [--yes]   # --yes: prod, after step 4
```

Run it in the background, logging to a file in your scratchpad, and wait for
the completion notification instead of polling. Its `==> ` lines mark the
phase: `gate`, `build`, `package`, `push images`, `GitHub release`,
`Homebrew formula`, `done`. Give the user a one-line update at each phase.

### 6. On failure

Report the phase, the exit code and the last error lines; then:

| failed in | state | next |
| --- | --- | --- |
| before `push images` (checks, gate, build, package) | nothing pushed | fix the cause, rerun from step 3 |
| `push images` | some image tags pushed | safe to rerun the same command (same tags overwrite) |
| `GitHub release` | images pushed, no release | rerun the same command |
| `Homebrew formula` | release exists | do NOT rerun (it stops at "release already exists"); the error / script prints the manual tap steps — offer them to the user |

Never delete a release, tag or image to recover without the user asking.

### 7. Report

From the `==> done` block: the three digest-pinned refs
(`…@sha256:…` — the values to pin in toolu.sh, never the tag), plus for prod
the release URL (`gh release view <tag> -R Falconiere/toolu-ghrunner --json url -q .url`)
and whether the Homebrew push ran. For prod, check whether `release.yml` fired:
`gh run list -R Falconiere/toolu-ghrunner --workflow release.yml -L 1` — tell the
user if it did.
