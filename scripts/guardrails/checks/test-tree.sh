# test-tree.sh — no centralized test directory.
#
# The companion to colocated-tests: that one catches a stray test FILE, this one
# catches the centralized test DIRECTORY it would land in, by any of its usual
# names. A directory is not something oxlint is ever asked to lint, so no linter
# rule can own this — which is why it has its own check id rather than riding
# along inside colocated-tests, where every TypeScript stack's `ownedByLinter`
# entry silently switched it off.

gr_check_test_tree() {
  mode=$1
  [ "$mode" = 'repo' ] || return 0

  for centralized in "$GR_SRC_ROOT/__tests__" "$GR_SRC_ROOT/tests" tests test; do
    [ "$centralized" = "$GR_SRC_ROOT/$GR_TEST_DIR" ] && continue
    # Rust's crate-root tests/ is the documented integration-test surface.
    [ "$centralized" = 'tests' ] && [ "$GR_TEST_DIR" = 'tests' ] && continue
    if [ -d "$centralized" ]; then
      gr_violation test-tree "$centralized" \
        'centralized test directory' \
        "colocate each test in a sibling $GR_TEST_DIR/ instead"
    fi
  done
}
