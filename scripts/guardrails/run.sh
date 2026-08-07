#!/usr/bin/env bash
# run.sh — the agent-guardrails entry point.
#
# Enforces the structural rules that a linter structurally cannot see: the
# folder tree, per-domain shape, colocated tests, barrels, banned dependencies,
# required files, committed secrets, and contextual code patterns. Anything
# oxlint / oxfmt / clippy / rustfmt already enforces stays with them — two
# enforcers of one rule is how ceilings drift apart.
#
#   run.sh                  repo mode  — every check, whole tree. The gate.
#   run.sh --file <path>…   scoped     — file-addressable checks for those paths
#   run.sh --hook           PostToolUse — reads hook JSON on stdin, then --file
#   run.sh --stop           Stop hook  — repo mode behind two early-outs
#   run.sh --only a,b       run only the named checks (used by the test suite)
#   run.sh --list           print every check id, one per line
#
# Exit: 0 clean · 1 violations · 2 violations in a hook mode · 3 misconfigured.
# Exit 2 in the hook modes is mandatory, not stylistic: Claude Code ignores a 1
# from a hook. Only 2 shows stderr to the agent on PostToolUse, and only 2
# blocks on Stop. A --stop that exited 1 would leave the whole layer inert while
# looking correctly wired up.
set -u

GR_DIR=$(cd "$(dirname "$0")" && pwd)
. "$GR_DIR/lib/report.sh"
. "$GR_DIR/lib/config.sh"

# Order is the order they run in. File-addressable checks first so --file mode
# short-circuits cheaply.
#
# A check id is the unit of ownership: `ownedByLinter` names ids, and declaring
# one skips the WHOLE check. So an id may not straddle what a linter can see and
# what it cannot — folder-readmes and test-tree are split out of folder-tree and
# colocated-tests for exactly that reason.
GR_CHECKS_FILE='folder-tree file-size colocated-tests no-barrels filename-case secret-content'
# Batch checks take EVERY path at once rather than one per call. ast-grep's
# process startup dwarfs the scan, so spawning it per staged file made a 20-file
# commit pay that cost 20 times — on the hook that fires for every single edit.
GR_CHECKS_BATCH='patterns'
GR_CHECKS_REPO='folder-readmes test-tree banned-deps shadow-configs required-files secrets'
GR_CHECKS_ALL="$GR_CHECKS_FILE $GR_CHECKS_BATCH $GR_CHECKS_REPO"

gr_list() {
  for check in $GR_CHECKS_ALL; do printf '%s\n' "$check"; done
}

# gr_selected <check> — honours --only, and skips whatever the linter owns.
#
# ownedByLinter is how "one rule, one enforcer" survives the split: oxlint's
# house plugin enforces the per-file rules on the TypeScript stacks, so this
# module must NOT also enforce them. The Rust stack lists nothing, because
# oxlint cannot parse Rust at all — there every check still runs here.
gr_selected() {
  case " $GR_OWNED_BY_LINTER " in *" $1 "*) return 1 ;; esac
  [ -z "$GR_ONLY" ] && return 0
  case ",$GR_ONLY," in *",$1,"*) return 0 ;; *) return 1 ;; esac
}

gr_call() {
  "gr_check_$(printf '%s' "$1" | tr '-' '_')" "${@:2}"
}

gr_run_checks() {
  mode=$1
  path=${2-}
  case "$mode" in
    repo) set_of_checks="$GR_CHECKS_FILE $GR_CHECKS_BATCH $GR_CHECKS_REPO" ;;
    file) set_of_checks=$GR_CHECKS_FILE ;;
  esac
  for check in $set_of_checks; do
    gr_selected "$check" || continue
    gr_call "$check" "$mode" "$path"
  done
}

# gr_run_file_mode — per-path checks once per path, batch checks once for all.
# The loop variable is gr_path, not `target`: the check helpers assign bare
# `target`/`base` names, so a plain `for target in …` here is clobbered mid-body
# — gr_ct_is_test sets target to the BASENAME, and every check after it then
# received "index.ts" instead of "src/ui/index.ts" and silently passed. The
# helpers now declare theirs `local`; the distinct name here is belt and braces.
gr_run_file_mode() {
  for gr_path in "${GR_PATHS[@]}"; do
    for check in $GR_CHECKS_FILE; do
      gr_selected "$check" || continue
      gr_call "$check" file "$gr_path"
    done
  done
  for check in $GR_CHECKS_BATCH; do
    gr_selected "$check" || continue
    gr_call "$check" batch "${GR_PATHS[@]}"
  done
}

GR_MODE=repo
# An array, not a space-joined string: a staged path may contain a space, and a
# string would split "src/my file.ts" into two paths that match nothing — the
# violation then silently vanishes. Unquoted expansion also glob-expands, so a
# path holding a `*` would fan out into whatever happened to be on disk.
GR_PATHS=()
GR_ONLY=''

while [ $# -gt 0 ]; do
  case "$1" in
    --list) gr_list; exit 0 ;;
    # --file takes one or more paths: Lefthook expands {staged_files} to every
    # staged file at once, so a single-path flag would break any real commit.
    --file)
      GR_MODE=file
      shift
      while [ $# -gt 0 ]; do
        case "$1" in --*) break ;; esac
        GR_PATHS+=("$1")
        shift
      done
      [ "${#GR_PATHS[@]}" -gt 0 ] || gr_fatal '--file needs at least one path'
      ;;
    --hook) GR_MODE=hook; shift ;;
    --stop) GR_MODE=stop; shift ;;
    --only) GR_ONLY=${2-}; [ -n "$GR_ONLY" ] || gr_fatal '--only needs a comma-separated check list'; shift 2 ;;
    *) gr_fatal "unknown flag: $1 (try --list)" ;;
  esac
done

gr_require_jq

# Workspace roots take a different path: there is no source tree here, so the
# per-package config never loads and the run fans out instead. Everything below
# is unchanged for a single-repo project, which is every project today.
if gr_is_workspace; then
  . "$GR_DIR/lib/workspace.sh"
  gr_load_workspace
  for check in $GR_WS_ROOT_CHECKS; do
    . "$GR_DIR/checks/$check.sh"
  done

  case "$GR_MODE" in
    repo) gr_ws_run_repo; exit $? ;;

    file)
      gr_ws_require_owned "${GR_PATHS[@]}"
      gr_ws_run_paths "${GR_PATHS[@]}"
      exit $?
      ;;

    hook)
      # The parent owns stdin — a child can no longer read a consumed payload,
      # so resolve the path here and hand the child --file instead.
      payload=$(cat)
      edited=$(printf '%s' "$payload" | jq -r '.tool_input.file_path // empty' 2>/dev/null)
      [ -n "$edited" ] || exit 0
      # An absolute path OUTSIDE the workspace survives this substitution
      # unchanged, and every check works repo-relative — so it would be tested
      # as a relative path that does not exist and exit 0. An edit we cannot
      # place is not an edit we can vouch for; say so rather than pass it.
      case "$edited" in
        "$PWD"/*) edited=${edited#"$PWD"/} ;;
        /*) gr_fatal "\"$edited\" is outside the workspace root ($PWD) — guardrails cannot place it in a package" ;;
      esac
      [ -e "$edited" ] || exit 0
      gr_ws_require_owned "$edited"
      gr_ws_run_paths "$edited"
      # A hook must exit 2, not 1: Claude Code ignores a 1 from a hook, so the
      # violation would never reach the agent. 3 stays 3 — misconfiguration is
      # not a violation.
      status=$?
      [ "$status" -eq 1 ] && exit 2
      exit "$status"
      ;;

    stop)
      payload=$(cat 2>/dev/null || printf '{}')
      active=$(printf '%s' "$payload" | jq -r '.stop_hook_active // false' 2>/dev/null)
      [ "$active" = 'true' ] && exit 0
      if git rev-parse --git-dir >/dev/null 2>&1; then
        [ -z "$(git status --porcelain 2>/dev/null)" ] && exit 0
      fi
      gr_ws_run_repo
      status=$?
      [ "$status" -eq 1 ] && exit 2
      exit "$status"
      ;;
  esac
fi

gr_load_config
gr_cache_config
for check in $GR_CHECKS_ALL; do
  . "$GR_DIR/checks/$check.sh"
done

case "$GR_MODE" in
  repo)
    gr_run_checks repo
    [ "$GR_FAIL" -eq 0 ] && exit 0 || exit 1
    ;;

  file)
    gr_run_file_mode
    [ "$GR_FAIL" -eq 0 ] && exit 0 || exit 1
    ;;

  hook)
    # PostToolUse payload on stdin. An Edit/Write that touched nothing we can
    # resolve is not an error — exit 0 and stay out of the way.
    payload=$(cat)
    edited=$(printf '%s' "$payload" | jq -r '.tool_input.file_path // empty' 2>/dev/null)
    [ -n "$edited" ] || exit 0
    # Hooks receive absolute paths; every check works repo-relative.
    edited=${edited#"$PWD"/}
    [ -e "$edited" ] || exit 0
    gr_run_checks file "$edited"
    [ "$GR_FAIL" -eq 0 ] && exit 0 || exit 2
    ;;

  stop)
    payload=$(cat 2>/dev/null || printf '{}')
    # Already continuing from a previous block — re-blocking on the same finding
    # just burns turns against the harness's 8-block ceiling.
    active=$(printf '%s' "$payload" | jq -r '.stop_hook_active // false' 2>/dev/null)
    [ "$active" = 'true' ] && exit 0
    # Nothing changed this turn: a conversational turn should never pay for the
    # gate. --porcelain rather than `git diff --quiet HEAD` on purpose — diff
    # ignores untracked files, and a brand-new file is exactly the case that
    # most needs checking.
    if git rev-parse --git-dir >/dev/null 2>&1; then
      [ -z "$(git status --porcelain 2>/dev/null)" ] && exit 0
    fi
    gr_run_checks repo
    [ "$GR_FAIL" -eq 0 ] && exit 0 || exit 2
    ;;
esac
