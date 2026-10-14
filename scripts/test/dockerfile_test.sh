#!/usr/bin/env bash
# dockerfile_test.sh — static validation of the Dockerfile's builder stage and
# stage order (#114).
#
# The builder compiles `toolu-runner` under BuildKit cache mounts so a local
# rebuild after a source edit reuses every unchanged compiled crate instead of
# recompiling all dependencies. Two properties make that safe, and both only
# show up as a broken or empty image much later, so they are pinned here:
#
#   1. The binary is copied OUT of the `target/` mount inside the same RUN. A
#      cache mount's contents are never part of the layer, so a `COPY --from=
#      builder` that reads `/src/target/...` would find nothing.
#   2. The `target/` mount is keyed by TARGETPLATFORM: `target/release` holds
#      one platform's artifacts, and two platforms built on one builder must
#      not share it. Every mount is `sharing=locked`, because cargo's own
#      package-cache lock lives outside them.
#
# Plus the stage-order rule the Dockerfile's header explains: `runner` is the
# LAST stage, so a bare `docker build .` produces the lean image.
#
# Every check is then re-run against a mutated copy with its property removed,
# and must fail there — a check that cannot fail proves nothing.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

DF="Dockerfile"
fail=0

if [[ ! -f "$DF" ]]; then
  echo "FAIL: $DF not found" >&2
  exit 1
fi

# Logical instructions: comment lines dropped, backslash continuations folded
# into one line, whitespace squeezed — so a check sees a whole RUN at once.
logical() {
  awk '
    /^[[:space:]]*#/ { next }
    { if (sub(/\\[[:space:]]*$/, "")) { buf = buf $0 " "; next }
      print buf $0; buf = "" }
  ' "$1" | tr -s ' \t' '  '
}

# The logical lines of stage `$2`: from its `FROM … AS $2` to the next FROM.
stage() {
  logical "$1" | awk -v s="$2" '/^FROM /{ on = ($NF == s) } on'
}

# run_checks FILE — print one "FAIL: <desc>" line per violated property.
run_checks() {
  local f="$1" builder runtime last_from build_run
  builder="$(stage "$f" builder)"
  runtime="$(stage "$f" runtime)"
  last_from="$(logical "$f" | grep '^FROM ' | tail -n 1)"
  build_run="$(grep -F 'cargo build --release --locked --bin toolu-runner' <<<"$builder")"

  [[ -n "$build_run" ]] ||
    echo "FAIL: builder runs cargo build --release --locked --bin toolu-runner"
  grep -Eq -- '--mount=type=cache,[^ ]*target=/usr/local/cargo/registry( |,)' <<<"$build_run" ||
    echo "FAIL: cargo registry cache mount"
  grep -Eq -- '--mount=type=cache,[^ ]*target=/usr/local/cargo/git( |,)' <<<"$build_run" ||
    echo "FAIL: cargo git cache mount"
  grep -Eq -- '--mount=type=cache,[^ ]*target=/src/target( |,)' <<<"$build_run" ||
    echo "FAIL: target/ cache mount"
  grep -Eq -- '--mount=type=cache,[^ ]*id=[^, ]*\$\{TARGETPLATFORM\}[^ ]*target=/src/target' <<<"$build_run" ||
    echo "FAIL: target/ mount keyed by TARGETPLATFORM"
  # cargo's package-cache lock sits at the CARGO_HOME root, outside these
  # mounts, so only BuildKit's lock keeps two builds out of each other's way.
  local dir
  for dir in /usr/local/cargo/registry /usr/local/cargo/git /src/target; do
    grep -Eq -- "--mount=type=cache,[^ ]*target=${dir}(,[^ ]*)?,sharing=locked( |,)|--mount=type=cache,[^ ]*sharing=locked,[^ ]*target=${dir}( |,)" <<<"$build_run" ||
      echo "FAIL: ${dir} mount is sharing=locked"
  done
  grep -Eq '^ARG TARGETPLATFORM$' <<<"$builder" ||
    echo "FAIL: builder declares ARG TARGETPLATFORM"
  grep -Fq 'target/release/toolu-runner /out/toolu-runner' <<<"$build_run" ||
    echo "FAIL: binary copied out of the mount in the build RUN"
  grep -Fxq 'COPY --from=builder /out/toolu-runner /usr/local/bin/toolu-runner' <<<"$runtime" ||
    echo "FAIL: runtime copies /out/toolu-runner to /usr/local/bin/toolu-runner"
  if logical "$f" | grep -Eq '^COPY --from=builder /src/target'; then
    echo "FAIL: nothing is copied from the target/ mount path"
  fi
  [[ "$last_from" =~ [[:space:]]AS[[:space:]]runner$ ]] ||
    echo "FAIL: runner is the last stage"
}

# --- the real Dockerfile ----------------------------------------------------
out="$(run_checks "$DF")"
if [[ -n "$out" ]]; then
  echo "$out" >&2
  fail=1
else
  echo "ok: $DF builder caches registry/git/target, copies the binary out, runner is last"
fi

# --- each check must be able to fail -----------------------------------------
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# mutate DESC SED-SCRIPT — apply the script to a copy and demand the named
# check fails. A script that no longer matches the file is itself a failure:
# otherwise a reworded Dockerfile would turn this tier into silent no-ops.
mutate() {
  local desc="$1" script="$2" m="$TMP/Dockerfile.mut"
  sed -e "$script" "$DF" >"$m"
  if cmp -s "$DF" "$m"; then
    echo "FAIL: mutation for '$desc' no longer changes $DF — update it" >&2
    fail=1
    return
  fi
  if grep -Fxq "FAIL: $desc" <<<"$(run_checks "$m")"; then
    echo "ok: removing it fails '$desc'"
  else
    echo "FAIL: '$desc' still passes with its property removed" >&2
    fail=1
  fi
}

mutate "cargo registry cache mount" 's#--mount=type=cache,id=toolu-cargo-registry,target=/usr/local/cargo/registry,sharing=locked##'
mutate "cargo git cache mount" 's#--mount=type=cache,id=toolu-cargo-git,target=/usr/local/cargo/git,sharing=locked##'
mutate "target/ cache mount" 's#target=/src/target#target=/tmp/elsewhere#'
mutate "target/ mount keyed by TARGETPLATFORM" 's#id=toolu-target-${TARGETPLATFORM}#id=toolu-target#'
mutate "/usr/local/cargo/registry mount is sharing=locked" 's#cargo/registry,sharing=locked#cargo/registry#'
mutate "/usr/local/cargo/git mount is sharing=locked" 's#cargo/git,sharing=locked#cargo/git#'
mutate "/src/target mount is sharing=locked" 's#target=/src/target,sharing=locked#target=/src/target#'
mutate "builder declares ARG TARGETPLATFORM" '/^ARG TARGETPLATFORM$/d'
mutate "binary copied out of the mount in the build RUN" 's#target/release/toolu-runner /out/toolu-runner#target/release/toolu-runner /src/toolu-runner#'
mutate "runtime copies /out/toolu-runner to /usr/local/bin/toolu-runner" 's#^COPY --from=builder /out/toolu-runner #COPY --from=builder /src/target/release/toolu-runner #'
mutate "nothing is copied from the target/ mount path" 's#^COPY --from=builder /out/toolu-runner .*#&\nCOPY --from=builder /src/target/release/toolu-runner /tmp/x#'
mutate "runner is the last stage" '$a\
FROM runtime AS extra'

if [[ "$fail" -ne 0 ]]; then
  echo "dockerfile_test: FAILED" >&2
  exit 1
fi
echo "dockerfile_test: all passed"
