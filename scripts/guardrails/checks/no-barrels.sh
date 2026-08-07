# no-barrels.sh — no re-export layer.
#
# With no barrel to hide behind, a grep for a symbol lands on its definition and
# an export nothing imports is genuinely dead — which is what makes knip
# accurate rather than merely enthusiastic.
#
# barrelExempt is per-stack and the difference is real: console exempts
# src/app/** because index.tsx is TanStack Router's name for a directory's "/"
# route, while expo exempts nothing at all — its router lives at app/ in the
# repo root, outside srcRoot, so every index.ts(x) under src/ is a true barrel.

gr_nb_exempt() {
  local target glob
  target=$1
  for glob in $GR_BARREL_EXEMPT; do
    # shellcheck disable=SC2254
    case "$target" in $glob) return 0 ;; esac
  done
  return 1
}

gr_nb_one() {
  local target base name problem remedy
  target=$1
  base=${target##*/}
  for name in $GR_BARREL_NAMES; do
    [ "$base" = "$name" ] || continue
    gr_nb_exempt "$target" && return 0
    # mod.rs gets its own remedy: Rust has no knip and no re-export/import fix
    # to point at — the actual move is renaming the file up a level next to
    # its folder (stacks/rust/STRUCTURE.md's "declare submodules from the
    # parent file", not the TS barrel's "delete and import concrete").
    case "$name" in
      mod.rs)
        problem='mod.rs barrel'
        remedy='rename it to a sibling file next to its folder (src/foo/mod.rs → src/foo.rs) and declare the submodules there'
        ;;
      *)
        problem='barrel file'
        remedy='delete it and import the concrete file — a re-export layer hides dead code from knip'
        ;;
    esac
    gr_violation no-barrels "$target" "$problem" "$remedy"
    return 0
  done
}

gr_check_no_barrels() {
  mode=$1
  path=${2-}
  [ -n "$GR_BARREL_NAMES" ] || return 0

  if [ "$mode" = 'file' ]; then
    # Scoped to srcRoot exactly like the repo-mode walk below, and like the
    # oxlint rule that replaced this on the TypeScript stacks. Unscoped, a hook
    # or a Lefthook run reads Expo's root-level app/index.tsx routes — which the
    # repo gate never looks at — as barrels.
    case "$path" in "$GR_SRC_ROOT"/*) ;; *) return 0 ;; esac
    gr_nb_one "$path"
    return 0
  fi

  [ -d "$GR_SRC_ROOT" ] || return 0
  while IFS= read -r f; do
    gr_nb_one "$f"
  done < <(find "$GR_SRC_ROOT" -type f ! -path '*/node_modules/*' 2>/dev/null)
}
