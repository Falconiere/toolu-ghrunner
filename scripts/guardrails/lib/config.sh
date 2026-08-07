# config.sh — locates, validates and reads guardrails.config.json.
#
# The config is the single source of truth for every ceiling and allowlist. It
# fails CLOSED: a missing key, an unknown key, or absent jq all exit 3 rather
# than falling back to a default. A guard rail that silently stops enforcing is
# worse than no guard rail, because you stop looking at it.

# Bumped when a change to the scripts requires the project to re-copy something.
# run.sh warns (never fails) when a project's copied config trails this.
#
# 2 — the shared lint base gained house/no-module-scope-database. A project that
#     re-copies lint/base.oxlintrc.json without also re-copying
#     scripts/guardrails/oxlint-plugin/ references a rule its plugin copy does
#     not export, and oxlint then fails at PLUGIN LOAD — the whole lint step,
#     not one rule. Workspace support alone would not have justified a bump
#     (it is additive); this does, because it couples the two.
GR_SCRIPTS_VERSION=2

GR_REQUIRED_KEYS='version srcRoot src fileSize functionSize testDir testGlob barrelNames bannedDeps shadowConfigs'
GR_OPTIONAL_KEYS='$schema barrelExempt requiredFiles secrets filenameCase ownedByLinter'

# The workspace manifest is a DIFFERENT document with a different contract: it
# names packages and carries the repo-level checks, and has no source tree of
# its own. Validating it against the per-package key set would reject it on
# every count — no srcRoot, no src, no ceilings — and the unknown-key rule
# below would read `packages` as a typo.
GR_WS_FILE='guardrails.workspace.json'
GR_WS_REQUIRED_KEYS='version packages'
GR_WS_OPTIONAL_KEYS='$schema bannedDeps secrets shadowConfigs requiredFiles'

gr_require_jq() {
  command -v jq >/dev/null 2>&1 || gr_fatal \
    'jq is required to read guardrails.config.json but was not found on PATH — install it (apt-get install jq · brew install jq · apk add jq)'
}

# gr_validate_keys <file> <required-keys> <optional-keys> — shared by both
# document kinds: the per-package guardrails.config.json and the workspace
# manifest. A missing required key or an unknown key is fatal; an unknown key is
# almost always a typo, and a typo'd key means the rule it was meant to
# configure silently stops applying.
#
# ONE jq call, not five. This used to spawn jq once per required key plus one
# for keys[] plus one for .version — ~9ms of process startup each, ~50ms per
# document. A workspace pays it twice, the manifest in the parent and the config
# in every child, on the hook that fires for every single Edit.
gr_validate_keys() {
  gr_vk_file=$1
  gr_vk_required=$2
  gr_vk_optional=$3

  # Three pipe-separated fields; the lists inside them are space-separated. The
  # field delimiter must NOT be whitespace: bash collapses runs of IFS
  # whitespace, so a line whose first field is empty would shift every value one
  # to the left and read the unknown-key list as the missing-key list.
  gr_vk_out=$(jq -r --arg req "$gr_vk_required" --arg opt "$gr_vk_optional" '
    if type != "object" then "__not_object__||0" else
      . as $doc
      | ($req | split(" ")) as $required
      | (($req + " " + $opt) | split(" ")) as $known
      | [ $required[] | . as $k | select($doc | has($k) | not) ] as $missing
      | [ $doc | keys[] | select(. as $k | $known | index($k) | not) ] as $unknown
      | ($missing | join(" ")) + "|" + ($unknown | join(" ")) + "|"
        + (($doc.version // 0) | tostring)
    end
  ' "$gr_vk_file" 2>/dev/null) || gr_fatal \
    "$gr_vk_file is not valid JSON — fix the syntax; the gate cannot run without it"

  IFS='|' read -r gr_vk_missing gr_vk_unknown gr_vk_version <<<"$gr_vk_out"

  [ "$gr_vk_missing" = '__not_object__' ] && gr_fatal \
    "$gr_vk_file is not a JSON object — the gate cannot read its settings"

  # Reported one at a time: the first missing key is the one to fix, and a list
  # of six reads as six problems rather than one unfinished file.
  for key in $gr_vk_missing; do
    gr_fatal "$gr_vk_file is missing required key: $key"
  done

  for key in $gr_vk_unknown; do
    gr_fatal "$gr_vk_file has unknown key: $key (typo? known keys: $gr_vk_required $gr_vk_optional)"
  done

  if [ "$gr_vk_version" -lt "$GR_SCRIPTS_VERSION" ] 2>/dev/null; then
    gr_warn "$gr_vk_file is version $gr_vk_version but these scripts are version $GR_SCRIPTS_VERSION — re-copy scripts/guardrails/ and the config from the kit"
  fi
}

# gr_is_workspace — is this a monorepo root? Sets GR_WS_MANIFEST when it is.
#
# An explicit $GR_CONFIG names one package's config, so it forces the
# single-repo path; that is also what lets a re-exec'd child never re-enter
# workspace mode.
gr_is_workspace() {
  GR_WS_MANIFEST=''
  # A re-exec'd child sets this. GR_CONFIG alone is not enough to stop
  # recursion: the child clears it to an EMPTY string so it resolves its own
  # config, and `[ -n "" ]` is false — so a package that somehow held a
  # manifest of its own would fan out again, forever. An explicit marker says
  # what is meant, and nesting is refused rather than silently recursed.
  [ -n "${GR_WS_CHILD-}" ] && return 1
  [ -n "${GR_CONFIG-}" ] && return 1
  [ -f "$GR_WS_FILE" ] || return 1
  # Both documents at the root is ambiguous — which one governs the checks that
  # exist in both? Refuse rather than pick. It also means a workspace can never
  # be created by dropping a manifest beside a config that oxlint would still
  # find and lint every package against.
  [ -f 'guardrails.config.json' ] && gr_fatal \
    "both guardrails.config.json and $GR_WS_FILE are present — a workspace root must have only the manifest, because the oxlint plugin resolves guardrails.config.json from the working directory and would lint every package against this one"
  GR_WS_MANIFEST=$GR_WS_FILE
  return 0
}

# gr_load_workspace — validate the manifest and cache the root-level values.
#
# GR_CONFIG_FILE points at the manifest on purpose: the four checks that run at
# the root (banned-deps, secrets, shadow-configs, required-files) read the same
# key names out of it that they read from a per-package config, so they run
# unmodified. The source-tree checks are NOT in that set — there is no srcRoot
# here for them to walk.
gr_load_workspace() {
  GR_CONFIG_FILE=$GR_WS_MANIFEST
  gr_validate_keys "$GR_CONFIG_FILE" "$GR_WS_REQUIRED_KEYS" "$GR_WS_OPTIONAL_KEYS"

  # gr_selected reads this, and `set -u` makes an unset one a hard error rather
  # than an empty match. There is no ownedByLinter at a workspace root: oxlint
  # runs per package, so it can never own a root-level check.
  GR_OWNED_BY_LINTER=''

  {
    read -r GR_WS_PACKAGES
    read -r GR_BANNED_DEPS
    read -r GR_SECRET_FILES
    read -r GR_SECRET_SCAN_EXEMPT
  } < <(jq -r '
    [ (.packages | join(" ")),
      ((.bannedDeps // []) | join(" ")),
      ((.secrets.neverTracked // []) | join(" ")),
      ((.secrets.scanExempt // []) | join(" "))
    ] | .[]
  ' "$GR_CONFIG_FILE")

  # An empty list is indistinguishable from a manifest nobody finished writing,
  # and it would make the whole gate pass while checking nothing.
  [ -n "$GR_WS_PACKAGES" ] || gr_fatal \
    "$GR_CONFIG_FILE lists no packages — a workspace that governs nothing passes every check while enforcing none"

  for gr_ws_pkg in $GR_WS_PACKAGES; do
    # run.sh cd's into each of these, so a traversal segment would take the
    # whole gate outside the repo and check a tree nobody is looking at. The
    # schema forbids it too, but nothing validates against the schema at gate
    # time — this is the enforcement.
    case "$gr_ws_pkg" in
      /*|*..*) gr_fatal \
        "$GR_CONFIG_FILE lists package \"$gr_ws_pkg\" — package paths must be repo-relative with no \"..\" segment" ;;
    esac
    [ -d "$gr_ws_pkg" ] || gr_fatal \
      "$GR_CONFIG_FILE lists package \"$gr_ws_pkg\" but no such directory exists — fix the path or drop the entry"
    [ -f "$gr_ws_pkg/guardrails.config.json" ] || gr_fatal \
      "package \"$gr_ws_pkg\" has no guardrails.config.json — every workspace package carries its own; copy one from the stack kit"
  done
}

gr_load_config() {
  GR_CONFIG_FILE=${GR_CONFIG:-guardrails.config.json}
  [ -f "$GR_CONFIG_FILE" ] || gr_fatal \
    "no config at $GR_CONFIG_FILE — copy guardrails.config.json from the stack kit, or set GR_CONFIG"
  gr_validate_keys "$GR_CONFIG_FILE" "$GR_REQUIRED_KEYS" "$GR_OPTIONAL_KEYS"
}

# gr_cache_config — read every value ONCE into shell variables.
#
# This is a performance requirement, not a tidiness one. Reading through jq
# per file turned a 500-file tree into 17 seconds of process spawns, and a gate
# that slow is a gate people route around. One jq call, then pure bash.
#
# Map-shaped values are flattened to "key|v1 v2;key2|v3;" and parsed with case
# rather than an associative array, which bash 3.2 (still the system bash on
# macOS) does not have.
gr_cache_config() {
  {
    read -r GR_SRC_ROOT
    read -r GR_FILE_MAX
    read -r GR_FN_MAX
    read -r GR_TEST_DIR
    read -r GR_TEST_GLOB
    read -r GR_TOPLEVEL_SET
    read -r GR_TOPLEVEL
    read -r GR_REQUIRE_README
    read -r GR_BARREL_NAMES
    read -r GR_BARREL_EXEMPT
    read -r GR_BANNED_DEPS
    read -r GR_SKIP_EXT
    read -r GR_SECRET_FILES
    read -r GR_SECRET_SCAN_EXEMPT
    read -r GR_NESTED_MAP
    read -r GR_SIZE_OVERRIDES
    read -r GR_FILENAME_CASE
    read -r GR_OWNED_BY_LINTER
  } < <(jq -r '
    [ .srcRoot,
      (.fileSize.max | tostring),
      (.functionSize.max | tostring),
      .testDir,
      .testGlob,
      (if (.src.topLevel == null) then "no" else "yes" end),
      ((.src.topLevel // []) | join(" ")),
      ((.src.requireReadme // []) | join(" ")),
      (.barrelNames | join(" ")),
      ((.barrelExempt // []) | join(" ")),
      (.bannedDeps | join(" ")),
      ((.fileSize.skipExtensions // []) | join(" ")),
      ((.secrets.neverTracked // []) | join(" ")),
      ((.secrets.scanExempt // []) | join(" ")),
      ((.src.nested // {}) | to_entries | map("\(.key)|\(.value | join(" "))") | join(";")),
      ((.fileSize.overrides // {}) | to_entries | map("\(.key)|\(.value)") | join(";")),
      ((.filenameCase // []) | map("\(.glob)|\(.regex)|\(.describe)") | join(";")),
      ((.ownedByLinter // []) | join(" "))
    ] | .[]
  ' "$GR_CONFIG_FILE")
}

# Deliberately no gr_cfg / gr_cfg_arr / gr_cfg_has / gr_match_glob helpers here.
# Every value is read once by gr_cache_config above and every check reads the
# cached variable, so a second jq-per-call accessor has no caller — and the
# "omitted vs empty array" distinction those helpers existed to express is
# already carried by GR_TOPLEVEL_SET. This directory is copied verbatim into
# every project, so an unused helper is not free: it is five more copies of code
# nothing exercises, in the one module whose whole point is that copies do not
# rot.
