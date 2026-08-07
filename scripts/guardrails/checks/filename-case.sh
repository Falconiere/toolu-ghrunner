# filename-case.sh — filenames follow the language's casing convention.
#
# Recovered from the Rust stack's script: nothing else enforces snake_case for
# .rs files. The TypeScript stacks omit the `filenameCase` key entirely, because
# oxlint's unicorn/filename-case already owns kebab-case there and two enforcers
# of one rule is the drift this module exists to prevent. Omitted key, no-op.

gr_fc_apply() {
  local glob regex describe mode path base f
  glob=$1
  regex=$2
  describe=$3
  mode=$4
  path=${5-}

  if [ "$mode" = 'file' ]; then
    # Scoped to srcRoot exactly like the repo-mode walk below: build.rs and the
    # root-level configs are not source, and the repo gate never sees them.
    case "$path" in "$GR_SRC_ROOT"/*) ;; *) return 0 ;; esac
    # shellcheck disable=SC2254
    case "${path##*/}" in $glob) ;; *) return 0 ;; esac
    printf '%s\n' "${path##*/}" | awk -v re="$regex" '$0 ~ re { found = 1 } END { exit !found }' && return 0
    gr_violation filename-case "$path" \
      "filename is not $describe" \
      "rename it — the convention is what makes a grep for a symbol land on its file"
    return 0
  fi

  [ -d "$GR_SRC_ROOT" ] || return 0
  # One grep for the whole tree rather than one per file.
  while IFS= read -r f; do
    base=${f##*/}
    # shellcheck disable=SC2254
    case "$base" in $glob) ;; *) continue ;; esac
    printf '%s\t%s\n' "$base" "$f"
  done < <(find "$GR_SRC_ROOT" -type f ! -path '*/node_modules/*' 2>/dev/null) \
    | awk -F'\t' -v re="$regex" '$1 !~ re { print $2 }' \
    | while IFS= read -r target; do
        [ -n "$target" ] || continue
        gr_violation filename-case "$target" \
          "filename is not $describe" \
          "rename it — the convention is what makes a grep for a symbol land on its file"
      done
}

gr_check_filename_case() {
  mode=$1
  path=${2-}
  [ -n "$GR_FILENAME_CASE" ] || return 0

  rest_rules=$GR_FILENAME_CASE
  while [ -n "$rest_rules" ]; do
    entry=${rest_rules%%;*}
    case "$rest_rules" in *\;*) rest_rules=${rest_rules#*;} ;; *) rest_rules='' ;; esac
    [ -n "$entry" ] || continue
    fc_glob=${entry%%|*}
    fc_tail=${entry#*|}
    gr_fc_apply "$fc_glob" "${fc_tail%%|*}" "${fc_tail#*|}" "$mode" "$path"
  done
}
