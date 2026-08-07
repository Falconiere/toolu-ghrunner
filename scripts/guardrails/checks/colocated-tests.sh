# colocated-tests.sh — a test FILE lives beside the code it exercises.
#
# A test in a sibling __tests__/ (TS) or tests/ (Rust) is found by whoever opens
# the file it covers. A centralized test tree is found by nobody, and drifts.
#
# The other half — a centralized test DIRECTORY existing at all — is the separate
# `test-tree` check, because it is a fact about the tree rather than about any
# file oxlint is asked to lint. Keeping the two under one id meant every stack
# that declares colocated-tests linter-owned also switched the directory scan
# off, and the oxlint rule never covered it.
#
# testGlob is load-bearing rather than cosmetic: marketing deliberately matches
# *.test.tsx as well as *.test.ts, because an interactive React island is a
# documented option there and a gate that only saw .test.ts would let a
# misplaced component test through.

gr_ct_is_test() {
  local target glob
  target=${1##*/}
  for glob in $GR_TEST_GLOB; do
    # shellcheck disable=SC2254
    case "$target" in $glob) return 0 ;; esac
  done
  return 1
}

gr_check_colocated_tests() {
  mode=$1
  path=${2-}
  if [ "$mode" = 'file' ]; then
    # Scoped to srcRoot exactly like the repo-mode walk below, and like the
    # oxlint rule that replaced this on the TypeScript stacks. Without it a hook
    # or a Lefthook run reaches root-level files the repo gate never looks at.
    case "$path" in "$GR_SRC_ROOT"/*) ;; *) return 0 ;; esac
    gr_ct_is_test "$path" || return 0
    case "$path" in
      *"/$GR_TEST_DIR/"*) return 0 ;;
      *) gr_violation colocated-tests "$path" \
           "test file outside a $GR_TEST_DIR/ directory" \
           "move it to a sibling $GR_TEST_DIR/ beside the file it covers" ;;
    esac
    return 0
  fi

  [ -d "$GR_SRC_ROOT" ] || return 0
  while IFS= read -r f; do
    gr_ct_is_test "$f" || continue
    case "$f" in
      *"/$GR_TEST_DIR/"*) ;;
      *) gr_violation colocated-tests "$f" \
           "test file outside a $GR_TEST_DIR/ directory" \
           "move it to a sibling $GR_TEST_DIR/ beside the file it covers" ;;
    esac
  done < <(find "$GR_SRC_ROOT" -type f ! -path '*/node_modules/*' 2>/dev/null)
}
