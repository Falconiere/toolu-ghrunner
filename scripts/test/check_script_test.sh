#!/usr/bin/env bash
# check_script_test.sh — static + behavioural validation of tools/check.sh.
#
# The regression this file exists for: git hands a pre-push hook the ref list on
# stdin and lefthook passes that stream through. cargo's target probe runs
# `rustc -`, which reads its crate source from stdin — so the leftover ref list
# was parsed as Rust and the gate died with E0762 instead of running clippy and
# the tests. Worse, cargo CACHES that failed probe in target/.rustc_info.json
# and replays it on every later invocation, so a single poisoned run wedges the
# gate until the cache file is deleted.
#
# `exec </dev/null` at the top of tools/check.sh is the fix. It only works if it
# runs BEFORE the first cargo invocation, which is what the ordering assertion
# below pins — a later edit that moves a cargo call above it, or drops the line,
# reopens the hole silently.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

SCRIPT="tools/check.sh"
fail=0

if [[ ! -f "$SCRIPT" ]]; then
  echo "FAIL: $SCRIPT not found" >&2
  exit 1
fi

# --- the stdin detach exists and precedes every cargo call ---

detach_line=$(grep -nE '^exec[[:space:]]+</dev/null[[:space:]]*$' "$SCRIPT" | head -1 | cut -d: -f1)
if [[ -z "$detach_line" ]]; then
  echo "FAIL: $SCRIPT does not detach stdin — add 'exec </dev/null' before any cargo call" >&2
  fail=1
else
  echo "ok: $SCRIPT detaches stdin (line $detach_line)"
fi

# Comment lines are skipped: the header block documents the mirrored `cargo fmt`
# / `cargo clippy` sequence in prose, and a doc line is not an invocation.
cargo_line=$(awk '
  /^[[:space:]]*#/                       { next }
  /(^|[^[:alnum:]_-])cargo[[:space:]]/   { print NR; exit }
' "$SCRIPT")
if [[ -z "$cargo_line" ]]; then
  echo "FAIL: $SCRIPT invokes no cargo command — the gate cannot be mirroring CI" >&2
  fail=1
elif [[ -n "$detach_line" ]] && ((detach_line >= cargo_line)); then
  echo "FAIL: stdin is detached at line $detach_line, after the first cargo call at line $cargo_line" >&2
  fail=1
else
  echo "ok: stdin is detached before the first cargo call (line $cargo_line)"
fi

# --- a git hook's repository variables are cleared before any cargo call ---
#
# Pushing from a linked worktree gives the pre-push hook an absolute GIT_DIR;
# tests that run `git commit` in temporary repositories then hit this
# checkout's index. The clear must precede the first cargo invocation.
clear_line=$(grep -nF 'git rev-parse --local-env-vars' "$SCRIPT" | grep -v '^[0-9]*:[[:space:]]*#' | head -1 | cut -d: -f1)
if [[ -z "$clear_line" ]]; then
  echo "FAIL: $SCRIPT does not clear the hook's GIT_* repository variables" >&2
  fail=1
elif [[ -n "$cargo_line" ]] && ((clear_line >= cargo_line)); then
  echo "FAIL: GIT_* variables are cleared at line $clear_line, after the first cargo call at line $cargo_line" >&2
  fail=1
else
  echo "ok: the hook's GIT_* repository variables are cleared before the first cargo call (line $clear_line)"
fi

# --- #118: the test step is nextest plus a separate doctest run ---
for pat in 'cargo nextest run --workspace' 'cargo test --workspace --doc'; do
  if grep -qF -- "$pat" "$SCRIPT"; then
    echo "ok: $SCRIPT runs '$pat'"
  else
    echo "FAIL: $SCRIPT does not run '$pat'" >&2
    fail=1
  fi
done

# --- a real run survives a poisoned stdin ---
#
# `no-allow` is the one group that finishes in milliseconds, so this pipes the
# exact byte stream git sends a pre-push hook into a REAL gate invocation rather
# than a stand-in. It asserts both halves of the failure mode: the group still
# passes, and nothing on stdin reaches a compiler.

refline='refs/heads/main 0000000000000000000000000000000000000000 refs/heads/main 0000000000000000000000000000000000000000'
out=$(printf '%s\n' "$refline" | "./$SCRIPT" no-allow 2>&1)
code=$?

if [[ "$code" -ne 0 ]]; then
  echo "FAIL: ./$SCRIPT no-allow exited $code with a git ref list on stdin" >&2
  printf '%s\n' "$out" >&2
  fail=1
else
  echo "ok: ./$SCRIPT no-allow passes with a git ref list on stdin"
fi

if printf '%s\n' "$out" | grep -qE 'E0762|character literal|failed to run `rustc`'; then
  echo "FAIL: stdin reached a compiler — ./$SCRIPT no-allow parsed the ref list as Rust" >&2
  printf '%s\n' "$out" >&2
  fail=1
else
  echo "ok: no compiler parsed the piped ref list"
fi

if [[ "$fail" -ne 0 ]]; then
  echo "check_script_test FAILED" >&2
  exit 1
fi

echo "check_script_test passed"
