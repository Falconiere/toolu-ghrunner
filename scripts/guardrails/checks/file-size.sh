# file-size.sh — the per-file code-line ceiling.
#
# Counts CODE lines only: blank lines and comment-only lines are excluded, the
# same way oxlint's max-lines does with skipBlankLines + skipComments. A file
# that is mostly documentation should not be punished for it.
#
# fileSize.skipExtensions lists the extensions a linter already owns, and on the
# TypeScript stacks that is .ts and .tsx — so this check is a deliberate no-op
# there. It does real work on marketing (.astro, which oxlint cannot see past
# the frontmatter, leaving this the only enforcer of an .astro file's size) and
# on rust (.rs). Two enforcers of one ceiling is exactly the drift this module
# exists to prevent.

# The counting program, shared by both modes. Emits "<count> <path>" per file
# and handles many files in ONE process — spawning an awk per file cost 3.6s on
# a 500-file tree, which is most of a gate budget spent on process startup.
GR_FS_AWK='
  FILENAME != prev { if (prev != "") print n + 0, prev; prev = FILENAME; n = 0; inblock = 0 }
  { line = $0
    sub(/^[ \t]+/, "", line)
    if (line == "") next
    if (line ~ /^\/\//) next          # // …
    # "# …" is a comment in shell/yaml/toml, but "#[derive(…)]" and "#![…]" are
    # Rust ATTRIBUTES — code, and dense in exactly the stack where this check is
    # the only enforcer. Skipping them undercounted every .rs file it measured.
    if (line ~ /^#/ && line !~ /^#!?\[/) next
    if (line ~ /^\/\*/) { inblock = 1 }
    if (inblock) { if (line ~ /\*\//) inblock = 0; next }
    if (line ~ /^\*/) next            # continuation of a /* … */ block
    if (line ~ /^---$/) next          # astro / frontmatter fences
    n++
  }
  END { if (prev != "") print n + 0, prev }
'

# gr_fs_ceiling_for <path> — first matching override, else fileSize.max. Reads
# the cached override map; no subprocess per file.
gr_fs_ceiling_for() {
  local target rest_map entry glob
  target=$1
  rest_map=$GR_SIZE_OVERRIDES
  while [ -n "$rest_map" ]; do
    entry=${rest_map%%;*}
    case "$rest_map" in *\;*) rest_map=${rest_map#*;} ;; *) rest_map='' ;; esac
    [ -n "$entry" ] || continue
    glob=${entry%%|*}
    # shellcheck disable=SC2254
    case "$target" in
      $glob) printf '%s' "${entry#*|}"; return ;;
    esac
  done
  printf '%s' "$GR_FILE_MAX"
}

gr_fs_skipped() {
  local ext
  for ext in $GR_SKIP_EXT; do
    case "$1" in *"$ext") return 0 ;; esac
  done
  return 1
}

# gr_fs_eligible <path> — is this file ours to measure at all?
gr_fs_eligible() {
  gr_fs_skipped "$1" && return 1
  # Tests are exempt: a thorough test file is a feature, not a smell. Both
  # shapes count — a nested sibling (src/parser/tests/lexer.rs) and Rust's
  # crate-root integration surface (tests/cli.rs), which has no leading slash
  # and so does not match the nested pattern.
  case "$1" in
    *"/$GR_TEST_DIR/"*) return 1 ;;
    "$GR_TEST_DIR"/*) return 1 ;;
  esac
  return 0
}

# gr_fs_judge — reads "<count> <path>" lines, reports what exceeds its ceiling.
gr_fs_judge() {
  while read -r lines target; do
    [ -n "$target" ] || continue
    ceiling=$(gr_fs_ceiling_for "$target")
    if [ "$lines" -gt "$ceiling" ]; then
      gr_violation file-size "$target" \
        "$lines code lines exceeds the ceiling of $ceiling" \
        'split it — promote the file to a folder and move the sub-parts into their own files'
    fi
  done
}

gr_check_file_size() {
  mode=$1
  path=${2-}

  if [ "$mode" = 'file' ]; then
    [ -f "$path" ] && gr_fs_eligible "$path" || return 0
    gr_fs_judge < <(awk "$GR_FS_AWK" "$path")
    return 0
  fi

  [ -d "$GR_SRC_ROOT" ] || return 0
  # Eligibility is decided in pure bash, then every surviving file is counted in
  # one awk pass. On the TypeScript stacks oxlint owns .ts/.tsx, so nothing
  # survives and the whole check costs a single find.
  list=$(mktemp)
  while IFS= read -r f; do
    gr_fs_eligible "$f" && printf '%s\n' "$f"
  done < <(find "$GR_SRC_ROOT" -type f ! -path '*/node_modules/*' 2>/dev/null) > "$list"

  if [ -s "$list" ]; then
    gr_fs_judge < <(tr '\n' '\0' < "$list" | xargs -0 awk "$GR_FS_AWK")
  fi
  rm -f "$list"
}
