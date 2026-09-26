#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
inputs="$root/crates/expressions/src/tests/expression_inputs.json"
reference="$root/crates/expressions/src/tests/expression_reference.json"
provenance="$root/crates/expressions/src/tests/expression_reference.provenance.json"
# These pins are authoritative: generated provenance must match them. Reading
# expected values from that same provenance would make this check tautological.
expected_sha=cab9d1c3901e45c7705889c4f88284fdd93f4ae5
expected_source_sdk_sha256=8031e25c0ad2e813cdbe1cce25d010a70387890d7d16a6e45b6bd28cdf8c1e45

python3 - "$inputs" "$reference" "$provenance" "$expected_sha" "$expected_source_sdk_sha256" <<'PY'
import hashlib
import json
import sys

inputs, reference, provenance, expected_sha, expected_source_sdk_sha256 = sys.argv[1:]
with open(inputs, encoding="utf-8") as source:
    expressions = json.load(source)
if not isinstance(expressions, list) or len(expressions) != 1402:
    raise SystemExit("expression input corpus must contain exactly 1402 expressions")
if len(set(expressions)) != len(expressions):
    raise SystemExit("expression input corpus contains duplicates")
for required in ("github.repository", "toJSON(github.array.*)", "format('{0:}', 'a')", "fromJSON('[1]') == fromJSON('[1]')"):
    if required not in expressions:
        raise SystemExit(f"expression input corpus is missing {required!r}")
with open(reference, encoding="utf-8") as source:
    rows = json.load(source)
if not isinstance(rows, list) or len(rows) != len(expressions):
    raise SystemExit("reference corpus row count differs from inputs")
for expression, row in zip(expressions, rows):
    if set(row) != {"expression", "kind", "value"} or row["expression"] != expression:
        raise SystemExit("reference corpus must preserve input order and row schema")
    if not isinstance(row["kind"], str) or not isinstance(row["value"], str):
        raise SystemExit("reference corpus kind and value must be strings")
with open(provenance, encoding="utf-8") as source:
    metadata = json.load(source)
if metadata.get("upstream_sha") != expected_sha:
    raise SystemExit("reference provenance has an unexpected upstream SHA")
if metadata.get("input_sha256") != hashlib.sha256(open(inputs, "rb").read()).hexdigest():
    raise SystemExit("reference provenance input hash differs from inputs")
if metadata.get("reference_sha256") != hashlib.sha256(open(reference, "rb").read()).hexdigest():
    raise SystemExit("reference provenance output hash differs from corpus")
if metadata.get("source_sdk_sha256") != expected_source_sdk_sha256:
    raise SystemExit("reference provenance has an unexpected SDK source hash")
if not metadata.get("dotnet_sdk_version") or not metadata.get("dotnet_runtime_version"):
    raise SystemExit("reference provenance lacks dotnet version details")
PY
