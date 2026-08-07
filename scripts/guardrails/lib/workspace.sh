# workspace.sh — monorepo dispatch: fan every mode out across the packages.
#
# The shape of the problem: all three wired invocations run with the working
# directory at the REPO ROOT — PostToolUse and Stop from .claude/settings.json,
# and lefthook's {staged_files}. A workspace root has no guardrails.config.json,
# so without this module gr_load_config would exit 3 on every one of them and
# enforcement layers 2 and 3 would be dead while looking correctly wired up.
#
# Why re-exec instead of a loop in this process: gr_cache_config reads every
# value ONCE into plain shell globals (GR_FILE_MAX, GR_TOPLEVEL, …). A second
# package handled in the same process would inherit the first one's ceilings and
# allowlists — and ownedByLinter differs per package, so the wrong checks would
# be skipped too. A child process starts from nothing, which is exactly the
# isolation we need. The cost is one fork per package; run-latency.sh holds it
# to the same budget as a single-root tree.
#
# The checks themselves are untouched. Each child runs with its working
# directory INSIDE its package, which is the single-root case they were written
# for.

# The root subset. Only the checks that read repo-level facts rather than a
# source tree — there is no srcRoot at a workspace root for the others to walk.
GR_WS_ROOT_CHECKS='banned-deps shadow-configs required-files secrets'

# gr_ws_owner <repo-relative-path> — the listed package containing this path,
# or empty. Longest match wins, so nested package dirs resolve to the innermost.
gr_ws_owner() {
  gr_ws_path=$1
  gr_ws_best=''
  for gr_ws_pkg in $GR_WS_PACKAGES; do
    case "$gr_ws_path" in
      "$gr_ws_pkg"/*)
        [ "${#gr_ws_pkg}" -gt "${#gr_ws_best}" ] && gr_ws_best=$gr_ws_pkg
        ;;
    esac
  done
  printf '%s' "$gr_ws_best"
}

# gr_ws_exec <package> [args…] — run the whole module inside one package.
#
# GR_CONFIG is cleared so the child resolves its own guardrails.config.json from
# its new working directory, and so gr_is_workspace refuses to recurse. Nesting
# is not supported and silently recursing would be worse than refusing.
# GR_PATH_PREFIX is what puts the package back into the reported path.
gr_ws_exec() {
  gr_ws_pkg=$1
  shift
  (
    cd "$gr_ws_pkg" || exit 3
    # Exported explicitly. A `VAR=x exec cmd` prefix does put VAR in the child's
    # environment in bash — verified — but it reads like it might not, and the
    # failure it would cause if it ever did not is infinite re-exec. Not worth
    # leaving to a subtlety of special-builtin assignment semantics.
    export GR_CONFIG='' GR_WS_CHILD=1 GR_PATH_PREFIX="$gr_ws_pkg/"
    exec "$GR_DIR/run.sh" "$@"
  )
}

# gr_ws_worst <current> <incoming> — 3 beats everything (a broken guard rail
# must never be mistakable for a clean run), then 2, then 1.
gr_ws_worst() {
  case "$1:$2" in
    3:*|*:3) printf '3' ;;
    2:*|*:2) printf '2' ;;
    1:*|*:1) printf '1' ;;
    *) printf '0' ;;
  esac
}

# gr_ws_run_root — the repo-level checks, against the manifest, in this process.
gr_ws_run_root() {
  for check in $GR_WS_ROOT_CHECKS; do
    gr_selected "$check" || continue
    gr_call "$check" repo
  done
}

# gr_ws_require_listed — no package may exist outside the manifest.
#
# Repo mode walks the packages the manifest names, so a package it forgot is
# not "clean", it is unvisited — and the gate reports green over a source tree
# nobody checked. A stray guardrails.config.json is the evidence that such a
# tree exists, and it is the one signal that cannot be produced by accident:
# somebody scaffolded a package and never wired it up.
gr_ws_require_listed() {
  while IFS= read -r gr_ws_found; do
    gr_ws_found=${gr_ws_found#./}
    gr_ws_dir=${gr_ws_found%/guardrails.config.json}
    [ -n "$(gr_ws_owner "$gr_ws_found")" ] && continue
    gr_fatal \
      "\"$gr_ws_dir\" has a guardrails.config.json but is not listed in $GR_WS_FILE — add it to packages, or delete the config; an unlisted package is never checked and the gate still reports green"
    # -prune, not -not -path: the filter form still DESCENDS into every pruned
    # directory before discarding the results, and this runs on every repo mode
    # — which means every Stop hook. .git and the build outputs are pruned for
    # the same reason, and dist/ additionally because a config copied into a
    # build artefact is not a package anyone needs to be warned about.
  done < <(find . \( -name node_modules -o -name .git -o -name dist -o -name build \
                   -o -name out -o -name coverage -o -name .wrangler -o -name .next \) -prune \
             -o -name guardrails.config.json -print 2>/dev/null)
}

# gr_ws_run_repo — every package, plus the root. Used by both repo and stop mode.
gr_ws_run_repo() {
  gr_ws_require_listed
  gr_ws_status=0
  for gr_ws_pkg in $GR_WS_PACKAGES; do
    gr_ws_exec "$gr_ws_pkg"
    gr_ws_status=$(gr_ws_worst "$gr_ws_status" "$?")
  done
  gr_ws_run_root
  [ "$GR_FAIL" -eq 0 ] || gr_ws_status=$(gr_ws_worst "$gr_ws_status" 1)
  return "$gr_ws_status"
}

# gr_ws_run_paths <path…> — bucket by owning package, then one child per bucket.
#
# Bucketing rather than one child per path: lefthook expands {staged_files} to
# every staged file at once, and a fork per file would pay the startup cost N
# times on the commit hook. Paths are rewritten package-relative because that is
# what the child's checks expect.
gr_ws_run_paths() {
  gr_ws_status=0
  for gr_ws_pkg in $GR_WS_PACKAGES; do
    gr_ws_bucket=()
    for gr_ws_p in "$@"; do
      [ "$(gr_ws_owner "$gr_ws_p")" = "$gr_ws_pkg" ] || continue
      gr_ws_bucket+=("${gr_ws_p#"$gr_ws_pkg"/}")
    done
    [ "${#gr_ws_bucket[@]}" -gt 0 ] || continue
    gr_ws_exec "$gr_ws_pkg" --file "${gr_ws_bucket[@]}"
    gr_ws_status=$(gr_ws_worst "$gr_ws_status" "$?")
  done
  return "$gr_ws_status"
}

# gr_ws_require_owned <path…> — an unowned path is either a root file (fine) or
# an unlisted package (fatal), and the two must not be confused.
#
# Rejecting every unowned path looks like the fail-closed choice and is not: a
# workspace root legitimately holds package.json, knip.json and the manifest
# itself, lefthook expands {staged_files} across whatever the commit touched, so
# staging any one of them would exit 3 and block every such commit. That is a
# broken hook, not a guard rail.
#
# The property actually worth keeping is that a package the manifest forgot
# never passes silently. So walk up from the path: a guardrails.config.json in
# any ancestor means a real package that is not listed, and that is still fatal.
# Nothing above it means a root-level file, and there is no file-addressable
# check at a workspace root to run against it — repo mode owns those.
gr_ws_require_owned() {
  for gr_ws_p in "$@"; do
    [ -n "$(gr_ws_owner "$gr_ws_p")" ] && continue
    # A path with no slash is a root file and has no ancestor to walk.
    case "$gr_ws_p" in */*) gr_ws_dir=${gr_ws_p%/*} ;; *) continue ;; esac
    while : ; do
      [ -f "$gr_ws_dir/guardrails.config.json" ] && gr_fatal \
        "\"$gr_ws_dir\" has a guardrails.config.json but is not listed in $GR_WS_FILE — add it to packages, or delete the config; an unlisted package is never checked and the gate still reports green"
      case "$gr_ws_dir" in */*) gr_ws_dir=${gr_ws_dir%/*} ;; *) break ;; esac
    done
  done
}
