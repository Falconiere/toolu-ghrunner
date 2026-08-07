# folder-readmes.sh — every indexed folder carries its README.md.
#
#   src.requireReadme  directories under srcRoot that must carry a README.md
#   src.nested "x/*"   every domain folder under x/ must carry one too, so a
#                      feature is self-describing the moment it exists
#
# Split out of folder-tree on purpose. A README is a fact about a file oxlint is
# never asked to lint, so no linter rule can ever own it — while it lived inside
# gr_check_folder_tree, the four TypeScript stacks (which all declare folder-tree
# in ownedByLinter) skipped it entirely and no README was enforced anywhere.
# Repo scope only: there is no single edited file whose README-ness to check.

gr_check_folder_readmes() {
  mode=$1
  [ "$mode" = 'repo' ] || return 0
  [ -d "$GR_SRC_ROOT" ] || return 0

  for name in $GR_REQUIRE_README; do
    if [ -d "$GR_SRC_ROOT/$name" ] && [ ! -f "$GR_SRC_ROOT/$name/README.md" ]; then
      gr_violation folder-readmes "$GR_SRC_ROOT/$name" \
        'missing README.md' \
        "add one from templates/folder-README.md — it is how an agent answers \"where does X live\" without reading the tree"
    fi
  done

  rest_map=$GR_NESTED_MAP
  while [ -n "$rest_map" ]; do
    entry=${rest_map%%;*}
    case "$rest_map" in *\;*) rest_map=${rest_map#*;} ;; *) rest_map='' ;; esac
    key=${entry%%|*}
    case "$key" in */\*) ;; *) continue ;; esac
    prefix=${key%/\*}
    [ -d "$GR_SRC_ROOT/$prefix" ] || continue
    for domain in "$GR_SRC_ROOT/$prefix"/*/; do
      [ -d "$domain" ] || continue
      [ -f "${domain}README.md" ] || gr_violation folder-readmes "${domain%/}" \
        'domain folder has no README.md' \
        'add one listing what belongs in this domain and what does not'
    done
  done
}
