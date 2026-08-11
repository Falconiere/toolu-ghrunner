# macOS runner image

The repo ships a second image for [toolu.sh](https://toolu.sh) so iOS and
macOS jobs (`xcodebuild`, simulators, fastlane) can run on **Namespace
macOS instances**. It is the same runner, the same zero-argument
environment contract, and the same exit codes as the Linux
[container image](container-image.md) — but a different *kind of
artifact*, and that difference drives everything below.

## What this image is (and is not)

It is **not** a macOS VM image. Namespace boots its **own** macOS base
image — macOS + Xcode + the Apple toolchain, selected per instance with
[shape selectors](https://namespace.so/docs/architecture/compute/macos)
— and there is no mechanism to supply a custom one. Customer code
arrives as an OCI image of which, in Namespace's words, *"only the image
filesystem is used; ENTRYPOINT/CMD from the image config are not
respected"*.

So `Dockerfile.macos` builds a `scratch` **carrier**: no OS, no shell,
nothing installed, just the payload that gets materialised onto the
instance and launched by name.

| Path in the image | What it is |
| --- | --- |
| `/entrypoint` | The launcher (`scripts/macos-entrypoint.sh`, 0755). The value the caller passes as `ApplicationRequest.command`. |
| `/toolu-runner` | The **darwin/arm64** runner binary, 0755. |
| `/node/<version>/` | Pre-seeded Node runtimes (20.18.3, 24.0.2 — darwin-arm64). |
| `/node/default` | The seeded version the launcher puts on `$PATH` (24.0.2). |

Everything else a job touches — `bash`, `git`, `curl`, `xcodebuild`,
simulators, `sudo`, Homebrew — comes from Namespace's base image, not
from here. That is also why there is no `apt-get` list to review: the
image installs nothing.

## Creating an instance

`CreateInstance` for macOS differs from the Linux path in three places:
`shape.os` / `shape.machineArch`, the base-image `selectors`, and
`applications[]` in place of `containers[]` (an application must name
its own `command`, since the image config is ignored). Connect-JSON, in
the shape `packages/api/src/providers/namespace/client.ts` already
builds for Linux:

```json
{
  "shape": {
    "virtualCpu": 6,
    "memoryMegabytes": 14336,
    "os": "macos",
    "machineArch": "arm64",
    "selectors": [
      { "name": "macos.version", "value": "26.x" }
    ]
  },
  "deadline": "2026-08-11T18:00:00.000Z",
  "documentedPurpose": "toolu.sh job <id>",
  "labels": [
    { "name": "toolu-org", "value": "<org>" },
    { "name": "toolu-job", "value": "<job id>" }
  ],
  "applications": [
    {
      "name": "runner",
      "imageRef": "ghcr.io/<owner>/toolu-ghrunner-macos@sha256:…",
      "command": "entrypoint",
      "envVars": [
        { "name": "TOOLU_JITCONFIG", "value": "<encoded_jit_config>" },
        { "name": "TOOLU_DEADLINE", "value": "1786471200000" }
      ]
    }
  ]
}
```

Notes on the fields that are easy to get wrong:

- **`command` is relative to the materialised image root** and is the
  only way to start anything — Namespace's own `macrun` example names a
  bare `entrypoint` the same way. `args` stays empty.
- **The JIT config travels in `envVars`, never `args`** — same rule as
  the Linux image: argv shows up in provider dashboards.
- **`selectors` picks the Xcode/macOS generation** (`macos.version=14.x`
  Sonoma, `15.x` Sequoia+Xcode 16, `26.x` Tahoe+Xcode 26, `27.x` Golden
  Gate; `image.with=xcode-26` / `xcode-beta` refine it). Omitting them
  takes whatever Namespace defaults to, which moves as they update
  images — pin the generation your jobs compile against.
- **Shape**: macOS shapes are `6x14`, `12x28`, `12x56` (arm64 only).
- The image's declared platform (`linux/arm64`) is **cosmetic**.
  Namespace reads the filesystem and ignores the config; its own example
  ships a darwin binary inside a `linux/amd64`-config image.

## Environment contract and exit codes

Identical to the Linux image — see
[container-image.md](container-image.md) for the full text of
`TOOLU_JITCONFIG` / `TOOLU_DEADLINE` and the watchdog's behaviour.

| Code | Meaning |
| --- | --- |
| `0` | Job completed Success/Skipped, or shutdown before any job was acquired. |
| `1` | Job completed Failure/Cancelled, or the listener failed. |
| `2` | Environment error before polling: no `TOOLU_JITCONFIG` **and** no registration, an unparseable JIT config, or a payload-less image. |
| `124` | The `TOOLU_DEADLINE` watchdog fired. |

Jobs report `RUNNER_OS=macOS` and `RUNNER_ARCH=ARM64`
(`shared::platform`), so `if: runner.os == 'macOS'` behaves.

## What the launcher does

`scripts/macos-entrypoint.sh` exists because nothing in this image is
installed — the three things the Linux `Dockerfile` does with `ENV` and
symlinks have to happen at start-up instead:

1. **Picks a writable data dir**: `TOOLU_RUNNER_HOME` if set, else
   `$HOME/.toolu-runner`, else `${TMPDIR:-/tmp}/toolu-runner`. The
   materialised image tree is not assumed writable.
2. **Wires the Node seed**: symlinks `/node/<version>` into
   `<data_dir>/node/<version>` (where
   `execution::node::runtime::node_cache_dir` looks for node-**type**
   actions) and prepends `/node/<default>/bin` to `$PATH` (what a
   `run: node …` step needs). Copying instead of linking would burn
   ~200 MB of instance disk per job for bytes that are already local.
3. **Selects the mode from the environment**:
   - `TOOLU_JITCONFIG` set → `toolu-runner boot` (one-shot; the
     provider path).
   - otherwise a registration under the data dir
     (`runners/<owner>/<repo>/config.toml` or the legacy
     `config.toml`) → `toolu-runner run` (the always-online loop; a
     persistent Mac).
   - neither → exit 2 naming both ways out.

Both branches `exec`, so the runner's own exit code is what the instance
reports. `scripts/test/macos_entrypoint_test.sh` runs each branch
against a real materialised tree.

## Limitations

- **No Docker on macOS.** `uses: docker://…`, job-level `container:`
  and `services:` are unsupported — same floor as the Linux image, and
  here it is the platform's, not a packaging choice.
- **arm64 only.** Namespace macOS instances are Apple Silicon; there is
  no x86_64 leg and no Rosetta assumption in the image.
- **Code signing is the job's problem.** The image ships no keychain,
  certificates or provisioning profiles; steps that sign must import
  them themselves (and the instance is destroyed afterwards).
- github.com JIT configs only, matching toolu.sh's minting path.

## Build, run, publish

Local build needs a darwin/arm64 binary in `dist/` first — Rust cannot
cross-compile to darwin without an Apple SDK, so that half only works on
a Mac:

```sh
# on a Mac
cargo build --release --locked --bin toolu-runner
mkdir -p dist && cp target/release/toolu-runner dist/toolu-runner-darwin-arm64
chmod +x dist/toolu-runner-darwin-arm64

# anywhere with buildx (the carrier compiles nothing)
docker buildx build --platform linux/arm64 -f Dockerfile.macos -t toolu-ghrunner-macos:dev .
```

To inspect exactly what an instance will see, materialise the tree the
way Namespace does:

```sh
docker buildx build --platform linux/arm64 -f Dockerfile.macos --output type=local,dest=out .
ls -l out            # entrypoint + toolu-runner must both be 0755
```

Publishing is tag-driven:
[`release-macos-image.yml`](../.github/workflows/release-macos-image.yml)
gates on fmt/clippy/tests, compiles the binary on `macos-14` (a real
Mac), packs it on `ubuntu-24.04` (GitHub's macOS runners have no Docker
daemon), pushes `ghcr.io/<owner>/toolu-ghrunner-macos:<version>` plus
`:latest` and the `:vN` compat line for stable tags, and prints the
digest-pinned ref:

```
ghcr.io/<owner>/toolu-ghrunner-macos@sha256:…
```

The tag rule, and why `vN` resets at 1.0.0, is the Linux image's rule —
see [container-image.md](container-image.md#tag-surface). Pin the
digest: this payload runs customer build code holding a live GitHub
credential.

## toolu.sh side (not done here)

This repo publishes the image; routing macOS jobs to it is a change in
`toolu.sh` that does **not** exist yet:

- a macOS branch in `providers/namespace/client.ts` (`applications[]`
  with `command`, the macOS shape, the selectors) — today
  `createNamespaceInstance` hardcodes `os: "linux"`,
  `machineArch: "amd64"` and `containers[]`;
- a second image pin (e.g. `NAMESPACE_MACOS_RUNNER_IMAGE`) beside
  `NAMESPACE_RUNNER_IMAGE`;
- a `runs-on` → platform decision, so only macOS-labelled jobs get a Mac
  (they cost multiples of a Linux instance).
