# required-files.sh — files whose absence is silent rather than loud.
#
# Recovered from the per-stack scripts this module replaces; dropping them in
# the migration would have been a regression:
#
#   src/pages/404.astro  without it, wrangler serves dead URLs as 200 with an
#                        empty body and search engines index them
#   wrangler.jsonc       without it the Worker has no deploy config at all
#
# Both fail at runtime, in production, rather than at build time — which is
# exactly the class of thing a structural gate is for.

gr_check_required_files() {
  mode=$1
  [ "$mode" = 'repo' ] || return 0

  count=$(jq -r '.requiredFiles // [] | length' "$GR_CONFIG_FILE")
  i=0
  while [ "$i" -lt "$count" ]; do
    target=$(jq -r ".requiredFiles[$i].path" "$GR_CONFIG_FILE")
    why=$(jq -r ".requiredFiles[$i].why" "$GR_CONFIG_FILE")
    [ -e "$target" ] || gr_violation required-files "$target" \
      'required file is missing' \
      "create it — $why"
    i=$((i + 1))
  done
}
