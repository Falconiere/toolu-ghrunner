---
name: deploy
description: Build every toolu-runner release artifact on this Mac and publish the release with scripts/deploy.sh — images, GitHub release, Homebrew. Run only when the user explicitly asks for a deploy.
argument-hint: --tag vX.Y.Z [--change-log]
disable-model-invocation: true
---

# deploy

Drive `scripts/deploy.sh` end to end for the user. The script does the work;
your job is the checks before it, the confirmation, the long-running watch,
and the report. Arguments given: `$ARGUMENTS`

What it pushes (from the script header — `scripts/deploy.sh --help`):
`ghcr.io/falconiere/toolu-ghrunner:<v>`, `:<v>-docker` and
`ghcr.io/falconiere/toolu-ghrunner-macos:<v>`, then the GitHub release
(prerelease when the tag has `-`), then the Homebrew formula (stable tags).
It never touches `:latest` / `:vN`.

The build, release and formula steps are the same scripts CI runs
(`scripts/build-release.sh`, `publish-release.sh`, `commit-homebrew-formula.sh`).
A deploy creates the tag; `release.yml`'s `published` job sees the
release and skips the CI run.

`<v>` is the tag minus its leading `v`.

## Rules

- **Never deploy on your own judgment.** The user must have asked for this
  deploy in this conversation AND said yes to the dry-run plan in step 4.
  Earlier approvals (another tag, another session) do not count.
- **Never bypass a failed check.** No `--no-verify`, no editing the script to
  skip the gate, no deleting a release or tag to "retry clean". Stop and report.
- **The script's confirm prompt reads `/dev/tty`, which you do not have.**
  Pass `--yes` only after the user's explicit yes in chat (step 4).
- One deploy at a time; never run two in parallel (they share `dist/`).

## Steps

### 1. Pin the inputs

- `tag`: must be `v` + `[workspace.package] version` from `Cargo.toml`.
  Check with `bash scripts/assert-version.sh <tag>` (exit 1 = mismatch →
  stop; bumping the version is the release-pr flow, not this skill).
- `--change-log`: release notes from git-cliff over the commits
  since the last tag, instead of the `CHANGELOG.md` `## [<v>]` section. If
  `bash scripts/changelog-extract.sh <v>` fails (no section) and the user did
  not ask for `--change-log`, ask which they want — do not pick for them.

### 2. Check the host (each line must pass; report the first failure)

```bash
uname -sm                                   # Darwin arm64 — anything else: stop, cannot build darwin
docker buildx version && docker info >/dev/null   # Docker Desktop, or Colima with rosetta: true
docker login ghcr.io </dev/null             # already logged in? (needs write:packages)
rustup --version && cargo --version
gh auth status
command -v git-cliff                        # --change-log only
```

The release also needs (the script re-checks all of these and stops on the first miss):

```bash
git status --porcelain                      # must be empty
git fetch -q origin main && git merge-base --is-ancestor HEAD origin/main
git ls-remote --tags origin refs/tags/<tag> # must print nothing (else release.yml owns it)
gh run list -R Falconiere/toolu-ghrunner --workflow ci.yml --commit "$(git rev-parse HEAD)" --status success
                                            # must list a run: CI is the gate, there is no local one
git config user.email                       # stable tags: the tap commit's identity
```

Write access to `Falconiere/homebrew-tap` (stable tags) is proven by the
script itself (clone + `git push --dry-run`) before anything is pushed; a
failure there names `gh auth setup-git`.

A missing login is the user's to fix: suggest `! docker login ghcr.io` /
`! gh auth login` (the `!` runs it in their session); never handle their token.

### 3. Dry run

```bash
scripts/deploy.sh --tag <tag> [--change-log] --dry-run
```

It prints the plan and every command, runs none. Exit 0 required; exit 2 is a
bad argument (message says which), exit 1 a version mismatch.

### 4. Confirm

Show the user, in a short list: tag, the three image refs, the release +
Homebrew lines and the notes source (the dry run's `notes:` line). Wait for
an explicit yes. Anything else → stop.

### 5. Run (long: 4 builds + 3 image pushes, often 30–60 min)

```bash
scripts/deploy.sh --tag <tag> [--change-log] --yes   # --yes only after step 4
```

Run it in the background, logging to a file in your scratchpad, and wait for
the completion notification instead of polling. Its `==> ` lines mark the
phase: `build`, `push images`, `GitHub release`, `Homebrew formula`,
`done`. Give the user a one-line update at each phase.

### 6. On failure

Report the phase, the exit code and the last error lines; then:

| failed in | state | next |
| --- | --- | --- |
| before `push images` (checks, build) | nothing pushed | fix the cause, rerun from step 3 |
| `push images` | some image tags pushed | safe to rerun the same command (same tags overwrite) |
| `GitHub release` | images pushed, no release | rerun the same command |
| `Homebrew formula` | release + tag exist | do NOT rerun (it stops at "tag already exists"); that error prints the manual tap steps — offer them to the user |

Never delete a release, tag or image to recover without the user asking.

### 7. Report

From the `==> done` block: the three digest-pinned refs
(`…@sha256:…` — the values to pin in toolu.sh, never the tag), plus
the release URL (`gh release view <tag> -R Falconiere/toolu-ghrunner --json url -q .url`)
and whether the Homebrew push ran. Confirm `release.yml` skipped:
`gh run list -R Falconiere/toolu-ghrunner --workflow release.yml -L 1` — its
`published` job should have succeeded and every other job been skipped; if not,
tell the user.
