#!/usr/bin/env bash
# release_scripts_test.sh — behaviour of the recipes release.yml /
# release-homebrew.yml and scripts/deploy.sh share:
#   scripts/build-release.sh, scripts/publish-release.sh,
#   scripts/commit-homebrew-formula.sh
#
# `cargo` and `gh` are stubs on PATH that record their argv (a release build
# and a GitHub release cannot run offline); tar, the package-*.sh scripts,
# sha256 and git run for real.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

fail=0
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
BIN="$tmp/bin"
CALLS="$tmp/calls"
export CALLS
mkdir -p "$BIN"

# cargo: records argv, then leaves the binary where `cargo build` would.
cat >"$BIN/cargo" <<'STUB'
#!/usr/bin/env bash
echo "cargo $*" >>"$CALLS"
triple="" bin=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --target) triple="$2"; shift 2 ;;
    --bin) bin="$2"; shift 2 ;;
    *) shift ;;
  esac
done
out="$CARGO_TARGET_DIR/${triple:+$triple/}release"
mkdir -p "$out"
printf '#!/bin/sh\necho %s\n' "$bin" >"$out/$bin"
chmod +x "$out/$bin"
STUB
cat >"$BIN/gh" <<'STUB'
#!/usr/bin/env bash
echo "gh $*" >>"$CALLS"
STUB
chmod +x "$BIN/cargo" "$BIN/gh"

check() { # <label> <condition...>
  local label="$1"
  shift
  if "$@"; then echo "ok: $label"; else echo "FAIL: $label" >&2; fail=1; fi
}

# --- build-release.sh ---
out="$tmp/out"
: >"$CALLS"
PATH="$BIN:$PATH" CARGO_TARGET_DIR="$tmp/target" bash scripts/build-release.sh linux amd64 "$out" >/dev/null
check "linux builds toolu-runner with --locked" grep -qxF -- "cargo build --release --locked --bin toolu-runner" "$CALLS"
check "linux builds toolu-daemon" grep -qxF -- "cargo build --release --locked --bin toolu-daemon" "$CALLS"
check "linux packages the runner tarball" test -f "$out/toolu-runner-linux-amd64.tar.gz"
check "linux packages the daemon tarball" test -f "$out/toolu-daemon-linux-amd64.tar.gz"

: >"$CALLS"
PATH="$BIN:$PATH" CARGO_TARGET_DIR="$tmp/target" bash scripts/build-release.sh darwin amd64 "$out" x86_64-apple-darwin >/dev/null
check "a triple becomes cargo --target" grep -qxF -- "cargo build --release --locked --bin toolu-runner --target x86_64-apple-darwin" "$CALLS"
check "the triple's binary is packaged" test -f "$out/toolu-runner-darwin-amd64.tar.gz"
check "darwin builds no daemon" test ! -e "$out/toolu-daemon-darwin-amd64.tar.gz"
check "darwin calls cargo once" test "$(wc -l <"$CALLS")" -eq 1

bash scripts/build-release.sh linux >/dev/null 2>&1
check "build-release usage error exits 2" test $? -eq 2

# --- publish-release.sh ---
notes="$tmp/notes.md"
echo "notes" >"$notes"
: >"$CALLS"
PATH="$BIN:$PATH" bash scripts/publish-release.sh v1.2.3-rc.1 "$out" "$notes" abc123 >/dev/null
check "SHA256SUMS verifies every tarball" bash -c "cd '$out' && sha256sum -c --quiet SHA256SUMS 2>/dev/null || shasum -a 256 -c --quiet SHA256SUMS"
check "SHA256SUMS covers the 3 tarballs" test "$(wc -l <"$out/SHA256SUMS")" -eq 3
check "a '-' tag is a prerelease at the given commit" grep -qxF -- "gh release create v1.2.3-rc.1 $out/toolu-runner-darwin-amd64.tar.gz $out/toolu-runner-linux-amd64.tar.gz $out/toolu-daemon-linux-amd64.tar.gz $out/SHA256SUMS --title v1.2.3-rc.1 --notes-file $notes --prerelease --target abc123" "$CALLS"
: >"$CALLS"
PATH="$BIN:$PATH" bash scripts/publish-release.sh v1.2.3 "$out" "$notes" >/dev/null
check "a stable tag is no prerelease and keeps gh's default target" grep -qxF -- "gh release create v1.2.3 $out/toolu-runner-darwin-amd64.tar.gz $out/toolu-runner-linux-amd64.tar.gz $out/toolu-daemon-linux-amd64.tar.gz $out/SHA256SUMS --title v1.2.3 --notes-file $notes" "$CALLS"
bash scripts/publish-release.sh v1.2.3 "$out" "$tmp/missing.md" >/dev/null 2>&1
check "publish-release without notes exits 2" test $? -eq 2

# --- commit-homebrew-formula.sh ---
sums="$tmp/SHA256SUMS"
for t in darwin-arm64 darwin-amd64 linux-amd64 linux-arm64; do
  printf '%064d  toolu-runner-%s.tar.gz\n' 7 "$t"
done >"$sums"
tap="$tmp/tap"
git init -q "$tap"
git -C "$tap" config user.name test
git -C "$tap" config user.email test@example.invalid
git -C "$tap" commit -q --allow-empty -m init
bash scripts/commit-homebrew-formula.sh v1.2.3 "$sums" "$tap" >/dev/null
check "a new formula is committed" test "$(git -C "$tap" log -1 --format=%s)" = "toolu-runner v1.2.3"
check "the formula carries the tag" grep -q "v1.2.3" "$tap/Formula/toolu-runner.rb"
bash scripts/commit-homebrew-formula.sh v1.2.3 "$sums" "$tap" >/dev/null
check "an unchanged formula exits 0" test $? -eq 0
check "an unchanged formula commits nothing" test "$(git -C "$tap" rev-list --count HEAD)" -eq 2
bash scripts/commit-homebrew-formula.sh v1.2.3 "$sums" "$tmp/not-a-repo" >/dev/null 2>&1
check "a tap dir that is not a git checkout exits 2" test $? -eq 2

exit "$fail"
