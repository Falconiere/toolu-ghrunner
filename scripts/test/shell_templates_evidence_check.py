#!/usr/bin/env python3
"""Check #80's evidence map without treating unverified live lanes as passing."""
import json
from pathlib import Path

root = Path(__file__).resolve().parents[2]
evidence = json.loads((root / "crates/execution/tests/shell_templates_evidence.json").read_text())
assert evidence["issue"] == 80
assert evidence["reference_source"] == "cab9d1c3901e45c7705889c4f88284fdd93f4ae5"
assert set(evidence["scenarios"]) == {f"80-S{number}" for number in range(1, 6)}
for scenario, proof in evidence["scenarios"].items():
    assert proof["expected"], scenario
    assert proof["command"], scenario
    for filename in proof["files"]:
        assert (root / filename).is_file(), filename
for lane in evidence["lanes"].values():
    assert lane["status"] in {"passed", "unverified", "not-applicable"}
    assert lane["evidence"], lane
for filename in ("README.md", "docs/test-coverage.md"):
    assert "shell" in (root / filename).read_text().lower()
print("#80 scenario map complete; lane statuses remain explicit")
