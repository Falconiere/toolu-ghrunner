#!/usr/bin/env bash
# tools/check.sh — THE gate for the toolu-runner Rust workspace. Rust only: no
# TypeScript, no marketing checks.
#
# One command, run before every push and mirrored step-for-step by
# .github/workflows/ci.yml:
#
#   cargo fmt --check
#   cargo clippy --all-targets -- -D warnings
#   bash scripts/guardrails/run.sh
#   cargo test
#
# The structure rules the compiler cannot see — the 500 code-line ceiling,
# snake_case filenames, mod.rs barrels, the folder tree, colocated tests,
# committed secrets, and the ast-grep pattern rules — belong to
# scripts/guardrails/ alone. One rule, one enforcer: this script must never
# grow a second copy of a ceiling that lives in guardrails.config.json, which
# is how the two numbers drift apart.

set -euo pipefail

(( BASH_VERSINFO[0] >= 4 )) || { printf 'toolu: bash 4+ required (got %s).\n' "$BASH_VERSION" >&2; exit 1; }

_self=${BASH_SOURCE[0]}
while [[ -L "$_self" ]]; do
  _d=$(cd -P "$(dirname "$_self")" && pwd); _self=$(readlink "$_self")
  [[ "$_self" != /* ]] && _self=$_d/$_self
done
_root=$(cd -P "$(dirname "$_self")" && pwd)
_project_root=$(cd "$_root/.." && pwd)

_check_rust() {
  (cd "$_project_root" && cargo fmt --all -- --check)
  (cd "$_project_root" && cargo clippy --workspace --all-targets -- -D warnings)
  _check_no_allow
  (cd "$_project_root" && bash scripts/guardrails/run.sh)
  (cd "$_project_root" && cargo test --workspace)
}

_check_no_allow() {
  # Reject #[allow(..)] / #[expect(..)] — must fix the lint, not silence it.
  # A house rule stricter than the kit's, with no counterpart in
  # scripts/guardrails/, so there is no second enforcer to drift from.
  #
  # `#!?\[` matches BOTH forms. The outer-only pattern this used to carry let a
  # crate-level `#![allow(clippy::unwrap_used)]` at the top of a file silence
  # the lint while the gate still reported clean — the one attribute that can
  # disable a rule for a whole file was the one the ban could not see.
  #
  # benches/ is the single sanctioned exception: a criterion harness needs
  # unwrap/expect, and clippy.toml's allow-unwrap-in-tests covers #[cfg(test)]
  # only — it does not reach a bench target. Nothing else is exempt.
  if grep -RnE '#!?\[(allow|expect)\(' \
       "$_project_root/crates" \
       --include='*.rs' \
       --exclude-dir=benches 2>/dev/null; then
    printf 'no-allow: #[allow(..)] / #[expect(..)] are not allowed (benches/ excepted)\n' >&2
    return 1
  fi
}

_usage() {
  cat <<'EOF'
toolu check — the quality gate (Rust only)

USAGE
  ./tools/check.sh GROUP

GROUPS
  all             fmt + clippy + no-allow + structure (guardrails) + test
  no-allow        just the #[allow(..)] / #[expect(..)] ban (CI calls this one
                  on its own, so the ban is enforced on every PR and not only
                  by the local pre-push hook)

REQUIREMENTS
  jq and ast-grep must be on PATH — scripts/guardrails/run.sh exits 3 without
  either, so a missing dependency can never look like a clean run.
  Install ast-grep with: cargo install ast-grep --locked
EOF
}

cmd=${1:-}
case "$cmd" in
  all)                _check_rust ;;
  no-allow)           _check_no_allow ;;
  help|''|-h|--help)  _usage ;;
  *)                  printf 'unknown group: %s\n' "$cmd" >&2; _usage; exit 2 ;;
esac
