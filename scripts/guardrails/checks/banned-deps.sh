# banned-deps.sh — a banned package cannot be installed and merely go unimported.
#
# oxlint's no-restricted-imports blocks these at the import site; this blocks
# them at the manifest. Both are needed: a dependency sitting in package.json
# with no import yet is a decision already made, and the next agent will use it.
#
# One HTTP client (the kit's own utilities/http.ts over fetch) and one validator
# (zod, CORE rule 13) — the bans exist to keep those singular.

gr_check_banned_deps() {
  mode=$1
  [ "$mode" = 'repo' ] || return 0

  banned=$GR_BANNED_DEPS
  [ -n "$banned" ] || return 0

  if [ -f package.json ]; then
    for dep in $banned; do
      if jq -e --arg d "$dep" \
        '((.dependencies // {}) + (.devDependencies // {}) + (.peerDependencies // {})) | has($d)' \
        package.json >/dev/null 2>&1; then
        gr_violation banned-deps package.json \
          "banned dependency: $dep" \
          'remove it — one HTTP client (utilities/http.ts over fetch) and one validator (zod)'
      fi
    done
  fi

  if [ -f Cargo.toml ]; then
    for dep in $banned; do
      if grep -Eq "^[[:space:]]*$dep[[:space:]]*=" Cargo.toml 2>/dev/null; then
        gr_violation banned-deps Cargo.toml \
          "banned dependency: $dep" \
          'remove it — see LIBRARIES.md for the reasoning behind each ban'
      fi
    done
  fi
}
