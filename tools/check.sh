#!/usr/bin/env bash
# tools/check.sh — code quality gates for the toolu-runner Rust
# workspace. Rust only: no TypeScript, no marketing checks.

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
  _check_file_size
  _check_no_allow
  _check_no_unwrap
}

_check_file_size() {
  # Reject .rs files > 700 lines (toolu default ceiling for Rust, slightly
  # relaxed to accommodate integration test harnesses). Function-body
  # complexity is enforced by clippy's `too_many_lines` (150).
  local fail=0
  while IFS= read -r f; do
    local n
    n=$(wc -l <"$f" | tr -d ' ')
    if (( n > 700 )); then
      printf 'file-size: %s is %s lines (> 700)\n' "$f" "$n" >&2
      fail=1
    fi
  done < <(find "$_project_root/crates" \
            -name '*.rs' -not -path '*/target/*')
  return $fail
}

_check_no_allow() {
  # Reject #[allow(..)] / #[expect(..)] — must fix the lint, not silence it.
  if grep -RnE '#\[(allow|expect)\(' \
       "$_project_root/crates" \
       --include='*.rs' 2>/dev/null; then
    printf 'no-allow: #[allow(..)] / #[expect(..)] are not allowed\n' >&2
    return 1
  fi
}

_check_no_unwrap() {
  # Reject .unwrap() / .expect("..") in production code (src/ tree).
  # Tests live in tests/ and use unwrap freely (clippy's
  # allow-unwrap-in-tests covers them).
  # For #[cfg(test)] mod tests inside src/ files, we extract each Rust
  # file, strip the lines after the `#[cfg(test)]` marker, then grep.
  local fail=0
  while IFS= read -r f; do
    # Find the first #[cfg(test)] marker; keep only the prefix.
    local prefix
    prefix=$(awk 'BEGIN{p=1} /^#\[cfg\(test\)\]/{exit} {print}' "$f")
    local matches
    matches=$(printf '%s\n' "$prefix" | grep -nE '\.(unwrap|expect)\(' || true)
    if [[ -n "$matches" ]]; then
      while IFS= read -r line; do
        printf '%s:%s\n' "$f" "$line" >&2
        fail=1
      done <<<"$matches"
    fi
  done < <(find "$_project_root/crates" -path '*/src/*.rs' 2>/dev/null)
  return $fail
}

_usage() {
  cat <<'EOF'
toolu check — code quality gates (Rust only)

USAGE
  ./tools/check.sh GROUP

GROUPS
  all             rust fmt + clippy + file-size + no-allow + no-unwrap
EOF
}

cmd=${1:-}
case "$cmd" in
  all)                _check_rust ;;
  help|''|-h|--help)  _usage ;;
  *)                  printf 'unknown group: %s\n' "$cmd" >&2; _usage; exit 2 ;;
esac
