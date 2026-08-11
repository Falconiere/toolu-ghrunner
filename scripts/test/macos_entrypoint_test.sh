#!/usr/bin/env bash
# macos_entrypoint_test.sh — behavioural test of scripts/macos-entrypoint.sh,
# the command a Namespace macOS instance runs (docs/macos-image.md).
#
# Nothing about this script can be checked by reading the image: it is only
# ever executed on a rented Mac, inside a job that already cost a minute of
# instance time to reach, and a wrong branch there looks like "the runner did
# nothing". So this builds a REAL image tree in a temp dir — the launcher, a
# stub `toolu-runner` that records how it was invoked, a stub node seed — and
# runs the launcher against it.
#
# The stub runner is the only fake, and it fakes exactly one thing: being a
# darwin binary. Everything it reports (argv, TOOLU_RUNNER_HOME, whether
# `node` resolved, the exit code it hands back) is the launcher's real output.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

SRC="scripts/macos-entrypoint.sh"
fail=0

if [[ ! -f "$SRC" ]]; then
  echo "FAIL: $SRC not found" >&2
  exit 1
fi
if [[ ! -x "$SRC" ]]; then
  echo "FAIL: $SRC is not executable — the image cannot chmod it (no shell in scratch)" >&2
  fail=1
fi

TMPROOT="$(mktemp -d)"
trap 'rm -rf "$TMPROOT"' EXIT

# image_tree <name> [node_default] — a materialised image filesystem: the
# launcher at /entrypoint, the stub runner beside it, optionally a node seed.
# Echoes the tree's path.
image_tree() {
  # Split declarations: `local a=$1 b=$a` evaluates every word before any
  # assignment, so the second would read an unset `a` under `set -u`.
  local name="$1"
  local node_default="${2:-}"
  local root="$TMPROOT/$name"
  mkdir -p "$root"
  cp "$SRC" "$root/entrypoint"
  chmod +x "$root/entrypoint"
  cat >"$root/toolu-runner" <<'STUB'
#!/usr/bin/env bash
# Stub runner: reports how the launcher invoked it, then exits with the code
# the case asked for (default 0) so exit-code propagation is observable.
echo "argv=$*"
echo "home=${TOOLU_RUNNER_HOME:-}"
echo "node=$(command -v node || echo none)"
exit "${STUB_EXIT:-0}"
STUB
  chmod +x "$root/toolu-runner"
  if [[ -n "$node_default" ]]; then
    mkdir -p "$root/node/$node_default/bin"
    printf '#!/usr/bin/env bash\necho stub-node\n' >"$root/node/$node_default/bin/node"
    chmod +x "$root/node/$node_default/bin/node"
    printf '%s\n' "$node_default" >"$root/node/default"
  fi
  echo "$root"
}

# check <desc> <expected> <actual>
check() {
  local desc="$1" want="$2" got="$3"
  if [[ "$want" == "$got" ]]; then
    echo "ok: $desc"
  else
    echo "FAIL: $desc" >&2
    echo "  want: $want" >&2
    echo "  got:  $got" >&2
    fail=1
  fi
}

# --- one-shot mode: TOOLU_JITCONFIG present -> `boot`, and never in argv ----
root="$(image_tree jit)"
home="$TMPROOT/home-jit"
out="$(TOOLU_RUNNER_HOME="$home" TOOLU_JITCONFIG="eyJmYWtlIjoxfQ==" "$root/entrypoint" 2>&1)"
code=$?
check "jitconfig -> boot"            "argv=boot" "$(grep '^argv=' <<<"$out")"
check "jitconfig -> exit 0"          "0"         "$code"
check "jitconfig stays out of argv"  ""          "$(grep -F 'eyJmYWtlIjoxfQ==' <<<"$out")"
check "data dir is honoured"         "home=$home" "$(grep '^home=' <<<"$out")"

# --- exit codes pass straight through (exec, not a wrapper) ----------------
out="$(STUB_EXIT=124 TOOLU_RUNNER_HOME="$TMPROOT/home-124" TOOLU_JITCONFIG=x "$root/entrypoint" >/dev/null 2>&1)"
check "the runner's exit code survives (124 = deadline)" "124" "$?"

# --- persistent mode: a per-repo registration -> `run` ---------------------
root="$(image_tree perrepo)"
home="$TMPROOT/home-perrepo"
mkdir -p "$home/runners/acme/app"
printf 'runner_id = 1\n' >"$home/runners/acme/app/config.toml"
out="$(TOOLU_RUNNER_HOME="$home" "$root/entrypoint" 2>&1)"
check "per-repo registration -> run" "argv=run" "$(grep '^argv=' <<<"$out")"

# --- persistent mode: a legacy root registration -> `run` ------------------
root="$(image_tree legacy)"
home="$TMPROOT/home-legacy"
mkdir -p "$home"
printf 'runner_id = 1\n' >"$home/config.toml"
out="$(TOOLU_RUNNER_HOME="$home" "$root/entrypoint" 2>&1)"
check "legacy root registration -> run" "argv=run" "$(grep '^argv=' <<<"$out")"

# --- neither -> exit 2, naming both ways out -------------------------------
root="$(image_tree bare)"
home="$TMPROOT/home-bare"
out="$(TOOLU_RUNNER_HOME="$home" "$root/entrypoint" 2>&1)"
code=$?
check "no jitconfig and no registration -> exit 2" "2" "$code"
if grep -q 'TOOLU_JITCONFIG' <<<"$out" && grep -q 'register' <<<"$out"; then
  echo "ok: the failure names both TOOLU_JITCONFIG and register"
else
  echo "FAIL: the exit-2 message must name both ways out; got: $out" >&2
  fail=1
fi

# --- node seed: linked into the data dir AND put on PATH -------------------
# Both halves matter and they are independent: the symlink is what
# `ensure_node_runtime` finds for node-TYPE actions, the PATH entry is what a
# `run: node …` step needs. Shipping one without the other is a silent
# regression in exactly one of the two.
#
# PATH is scrubbed to the system dirs for these cases: this host has a `node`
# of its own, and inheriting it would make "the seed reached $PATH" pass for
# the wrong reason and "no bogus node is exported" fail for one.
root="$(image_tree seeded 24.0.2)"
home="$TMPROOT/home-seeded"
out="$(PATH=/usr/bin:/bin TOOLU_RUNNER_HOME="$home" TOOLU_JITCONFIG=x "$root/entrypoint" 2>&1)"
check "seeded node lands on PATH" "node=$root/node/24.0.2/bin/node" "$(grep '^node=' <<<"$out")"
if [[ -L "$home/node/24.0.2" && -x "$home/node/24.0.2/bin/node" ]]; then
  echo "ok: the seed is linked at <data_dir>/node/<version>"
else
  echo "FAIL: expected a symlink at $home/node/24.0.2 resolving to the seeded runtime" >&2
  fail=1
fi

# Re-running against the same data dir must not fail on the existing link —
# a persistent Mac restarts this launcher on every supervisor restart.
out="$(TOOLU_RUNNER_HOME="$home" TOOLU_JITCONFIG=x "$root/entrypoint" 2>&1)"
check "a second run over the same data dir still boots" "argv=boot" "$(grep '^argv=' <<<"$out")"

# …and the harder half of that: the data dir outlives the IMAGE, so after an
# image upgrade the link points at a mount that no longer exists. A dangling
# link reads as absent to `-e` but exists to `ln`, so a guarded link would die
# with "File exists" under `set -e` — before dispatching anything, which looks
# like a runner that did nothing at all.
ln -sfn "$TMPROOT/gone/node/24.0.2" "$home/node/24.0.2"
out="$(PATH=/usr/bin:/bin TOOLU_RUNNER_HOME="$home" TOOLU_JITCONFIG=x "$root/entrypoint" 2>&1)"
check "a stale seed link from a previous image is repaired" "argv=boot" "$(grep '^argv=' <<<"$out")"
check "…and repointed at this image's seed" "$root/node/24.0.2" "$(readlink "$home/node/24.0.2")"

# A REAL directory at that path is an engine-downloaded runtime
# (`ensure_node_runtime` extracts into <data_dir>/node/<version>), and it must
# survive: `ln -sfn` against a real directory does not replace it, it links
# INSIDE it, which would leave the path without a `bin/node` and break exactly
# the lookup the seed exists to serve.
home="$TMPROOT/home-realnode"
mkdir -p "$home/node/24.0.2/bin"
printf '#!/usr/bin/env bash\necho downloaded-node\n' >"$home/node/24.0.2/bin/node"
chmod +x "$home/node/24.0.2/bin/node"
out="$(PATH=/usr/bin:/bin TOOLU_RUNNER_HOME="$home" TOOLU_JITCONFIG=x "$root/entrypoint" 2>&1)"
check "an engine-downloaded runtime still boots" "argv=boot" "$(grep '^argv=' <<<"$out")"
if [[ -d "$home/node/24.0.2" && ! -L "$home/node/24.0.2" && -x "$home/node/24.0.2/bin/node" && ! -e "$home/node/24.0.2/24.0.2" ]]; then
  echo "ok: the downloaded runtime is left intact, with no link nested inside it"
else
  echo "FAIL: $home/node/24.0.2 must stay a real directory with bin/node and no nested link" >&2
  ls -la "$home/node/24.0.2" >&2
  fail=1
fi

# --- a seed whose `default` names a missing runtime warns, does not die ----
root="$(image_tree brokenseed 24.0.2)"
printf '99.0.0\n' >"$root/node/default"
home="$TMPROOT/home-brokenseed"
out="$(PATH=/usr/bin:/bin TOOLU_RUNNER_HOME="$home" TOOLU_JITCONFIG=x "$root/entrypoint" 2>&1)"
check "an unusable node default still boots the job" "argv=boot" "$(grep '^argv=' <<<"$out")"
check "…and no bogus node is exported"               "node=none" "$(grep '^node=' <<<"$out")"

# --- data dir defaults to $HOME/.toolu-runner when unset -------------------
root="$(image_tree defaulthome)"
fakehome="$TMPROOT/fakehome"
mkdir -p "$fakehome"
out="$(env -u TOOLU_RUNNER_HOME HOME="$fakehome" TOOLU_JITCONFIG=x "$root/entrypoint" 2>&1)"
check "unset data dir defaults under \$HOME" "home=$fakehome/.toolu-runner" "$(grep '^home=' <<<"$out")"

# --- a broken image (no binary) fails as an env error, not a crash ---------
root="$TMPROOT/nobinary"
mkdir -p "$root"
cp "$SRC" "$root/entrypoint"
chmod +x "$root/entrypoint"
out="$(TOOLU_RUNNER_HOME="$TMPROOT/home-nobinary" TOOLU_JITCONFIG=x "$root/entrypoint" 2>&1)"
check "a payload-less image exits 2" "2" "$?"

if [[ "$fail" -ne 0 ]]; then
  echo "macos_entrypoint_test: FAILED" >&2
  exit 1
fi
echo "macos_entrypoint_test: all passed"
