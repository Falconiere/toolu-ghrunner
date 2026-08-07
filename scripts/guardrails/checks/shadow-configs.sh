# shadow-configs.sh — a config file that silently overrides another.
#
# Every entry here is a bug that costs an afternoon to find, because the
# symptom is "the tool ran and did nothing":
#
#   lefthook.yaml    shadowed by the lefthook 2.x installer's stub, so the
#                    pre-commit hooks never run and commits sail through
#   vitest.config.ts REPLACES vite.config.ts for test runs rather than merging
#                    with it, so the router plugin and the @/* alias vanish
#                    under test and route imports fail with a resolution error
#   wrangler.toml    beside a wrangler.jsonc — the classic way to deploy one
#                    thing and test another

gr_check_shadow_configs() {
  mode=$1
  [ "$mode" = 'repo' ] || return 0

  count=$(jq -r '.shadowConfigs | length' "$GR_CONFIG_FILE")
  i=0
  while [ "$i" -lt "$count" ]; do
    found=$(jq -r ".shadowConfigs[$i].found" "$GR_CONFIG_FILE")
    use=$(jq -r ".shadowConfigs[$i].use" "$GR_CONFIG_FILE")
    why=$(jq -r ".shadowConfigs[$i].why" "$GR_CONFIG_FILE")
    if [ -e "$found" ]; then
      gr_violation shadow-configs "$found" \
        "shadows $use ($why)" \
        "delete $found and keep the configuration in $use"
    fi
    i=$((i + 1))
  done
}
