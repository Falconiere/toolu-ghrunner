#!/usr/bin/env python3
"""Fail closed when issue #99 evidence is stale or a required lane lacks passing evidence."""

import argparse
import hashlib
import json
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
# A test attribute, then any further attributes or doc lines, then the fn.
TEST_FN = r"#\[(?:tokio::)?test\b[^\]]*\]\s*(?:(?:#\[[^\]]*\]|///[^\n]*)\s*)*(?:async\s+)?fn {}\("


PLACEHOLDER = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-5[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}")
CREDENTIAL = re.compile(r"\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_\w{20,}|eyJ[\w-]{10,}\.[\w-]{10,})")


def sha256(path):
    return hashlib.sha256((ROOT / path).read_bytes()).hexdigest()


def strings(value):
    if isinstance(value, str):
        yield value
    elif isinstance(value, dict):
        for item in value.values():
            yield from strings(item)
    elif isinstance(value, list):
        for item in value:
            yield from strings(item)


def assert_sanitized(path):
    message = json.loads((ROOT / path).read_text())
    for name, variable in message["variables"].items():
        if variable.get("isSecret"):
            assert PLACEHOLDER.fullmatch(variable["value"]), f"{path}: secret {name} is not a placeholder"
    for hint in message["mask"]:
        assert PLACEHOLDER.fullmatch(hint["value"]), f"{path}: mask value is not a placeholder"
    for endpoint in message["resources"]["endpoints"]:
        for key, value in endpoint.get("authorization", {}).get("parameters", {}).items():
            assert value == "redacted", f"{path}: {endpoint['name']} {key} is not redacted"
    leaked = [text[:12] for text in strings(message) if CREDENTIAL.search(text)]
    assert not leaked, f"{path}: credential-shaped values {leaked}"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--local", action="store_true", help="Validate fixture/test mapping only")
    args = parser.parse_args()
    data = json.loads((ROOT / "crates/execution/tests/step_attrs_evidence.json").read_text())
    assert data["issue"] == 99
    assert data["official_runner_commit"] == "cab9d1c3901e45c7705889c4f88284fdd93f4ae5"
    assert (ROOT / data["workflow"]).is_file()
    for path, digest in {**data["captures"], **data["actions"]}.items():
        assert sha256(path) == digest, f"{path} changed; re-record its evidence"
    for path in data["captures"]:
        assert_sanitized(path)
    workflow = (ROOT / data["workflow"]).read_text()
    for ref, local_dir in data["pinned_actions"].items():
        assert re.search(rf"uses: {re.escape(ref)}(\s|$)", workflow), \
            f"{data['workflow']} no longer uses {ref}"
        commit = ref.rsplit("@", 1)[1]
        for path, digest in data["actions"].items():
            if not path.startswith(local_dir + "/"):
                continue
            pinned = subprocess.run(["git", "-C", str(ROOT), "show", f"{commit}:{path}"],
                                    capture_output=True, check=False)
            assert pinned.returncode == 0, (
                f"git show {commit}:{path} failed: {pinned.stderr.decode().strip()} "
                f"(if the commit is missing, run `git fetch origin {commit}`)")
            assert hashlib.sha256(pinned.stdout).hexdigest() == digest, \
                f"{path} differs from the pinned {commit}; re-pin the workflow"
    assert set(data["scenarios"]) == {f"S{i}" for i in range(1, 6)}
    covered = set()
    for name, scenario in data["scenarios"].items():
        assert scenario["expected"] and scenario["live"], name
        for path, test in scenario["tests"]:
            source = (ROOT / path).read_text()
            assert re.search(TEST_FN.format(re.escape(test)), source), \
                f"{name}: {path}::{test} is not a #[test] / #[tokio::test] fn"
        covered.update(scenario["ac"])
    assert covered == {f"AC-{i}" for i in range(1, 6)}
    if args.local:
        print("Local fixture/test mapping valid; this does not certify execution or live parity.")
        return
    missing = [name for name, lane in data["required_live"].items()
               if lane["status"] != "passed" or not lane["evidence"]]
    if missing:
        raise SystemExit("Unverified required lanes: " + ", ".join(missing))
    print("Required lane evidence is recorded; verify its linked observations before closure.")


if __name__ == "__main__":
    main()
