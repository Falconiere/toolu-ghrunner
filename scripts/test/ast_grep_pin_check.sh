#!/usr/bin/env bash
# ast_grep_pin_check.sh — verify the ast-grep pin in .github/workflows/ci.yml.
#
# Reads AST_GREP_VERSION / AST_GREP_SHA256 from the `ci` job, downloads the real
# release asset, checks the SHA-256, confirms the binary reports that version,
# and runs the structure guardrails with it as the only ast-grep on PATH. Needs
# network and unzip; deliberately NOT in CI (the workflow's own install step
# performs the same download and checksum). Run it when bumping the pin.
set -euo pipefail

cd "$(dirname "$0")/../.." || exit 1 # repo root

WF=".github/workflows/ci.yml"
version=$(sed -n 's/^ *AST_GREP_VERSION: "\([^"]*\)".*/\1/p' "$WF")
sha=$(sed -n 's/^ *AST_GREP_SHA256: \([0-9a-f]\{64\}\).*/\1/p' "$WF")
if [[ -z "$version" || -z "$sha" ]]; then
  echo "FAIL: could not read the ast-grep pin from $WF" >&2
  exit 1
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

curl --fail --silent --show-error --location --retry 3 -o "$tmp/ast-grep.zip" \
  "https://github.com/ast-grep/ast-grep/releases/download/${version}/app-x86_64-unknown-linux-gnu.zip"
echo "${sha}  $tmp/ast-grep.zip" | sha256sum --check --strict -
unzip -q "$tmp/ast-grep.zip" -d "$tmp"

reported=$("$tmp/ast-grep" --version)
if [[ "$reported" != "ast-grep ${version}" ]]; then
  echo "FAIL: binary reports '$reported', pin says ${version}" >&2
  exit 1
fi
echo "ok: checksum and version match the pin (${version})"

# Only the pinned binary and the system tools; no other ast-grep can leak in.
jq_dir=$(dirname "$(command -v jq)")
PATH="$tmp:$jq_dir:/usr/bin:/bin" bash scripts/guardrails/run.sh
echo "ok: guardrails pass with the pinned ast-grep"
