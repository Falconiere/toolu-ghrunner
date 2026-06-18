#!/usr/bin/env bash
# tools/check.sh — code quality gates for toolu-runner.
#
# Mirrors a subset of yamless's ./tools/yamless/check.sh but scoped to
# the toolu-runner Rust workspace. No TypeScript, no marketing checks.

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
  _check_no_yamless
}

_check_file_size() {
  # Reject .rs files > 150 lines (matches yamless clippy.toml setting).
  local fail=0
  while IFS= read -r f; do
    local n
    n=$(wc -l <"$f" | tr -d ' ')
    if (( n > 150 )); then
      printf 'file-size: %s is %s lines (> 150)\n' "$f" "$n" >&2
      fail=1
    fi
  done < <(find "$_project_root/shared" "$_project_root/protocol" "$_project_root/toolu-runner" \
            -name '*.rs' -not -path '*/target/*')
  return $fail
}

_check_no_allow() {
  # Reject #[allow(..)] / #[expect(..)] — must fix the lint, not silence it.
  if grep -RnE '#\[(allow|expect)\(' \
       "$_project_root/shared" "$_project_root/protocol" "$_project_root/toolu-runner" \
       --include='*.rs' 2>/dev/null; then
    printf 'no-allow: #[allow(..)] / #[expect(..)] are not allowed\n' >&2
    return 1
  fi
}

_check_no_unwrap() {
  # Reject .unwrap() / .expect("..") outside tests.
  if grep -RnE '\.(unwrap|expect)\(' \
       "$_project_root/shared" "$_project_root/protocol" "$_project_root/toolu-runner" \
       --include='*.rs' 2>/dev/null \
       | grep -vE '(test|#\[cfg\(test\)\]|/\\* test \\*/)'; then
    printf 'no-unwrap: .unwrap() / .expect() are not allowed in production code\n' >&2
    return 1
  fi
}

_check_no_yamless() {
  # AC #19: no yamless coupling in source.
  if grep -RnE 'yamless|YAMLESS_' \
       "$_project_root/shared" "$_project_root/protocol" "$_project_root/toolu-runner" \
       "$_project_root/Cargo.toml" \
       --include='*.rs' --include='*.toml' 2>/dev/null; then
    printf 'no-yamless: yamless / YAMLESS_ references in source\n' >&2
    return 1
  fi
}

_usage() {
  cat <<'EOF'
toolu check — code quality gates (Rust only)

USAGE
  ./tools/check.sh GROUP

GROUPS
  all             rust fmt + clippy + file-size + no-allow + no-unwrap + no-yamless
EOF
}

cmd=${1:-}
case "$cmd" in
  all)                _check_rust ;;
  help|''|-h|--help)  _usage ;;
  *)                  printf 'unknown group: %s\n' "$cmd" >&2; _usage; exit 2 ;;
esac
