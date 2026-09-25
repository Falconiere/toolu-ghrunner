#!/usr/bin/env python3
"""Fail closed when required issue #100 acceptance lanes lack passing evidence."""

import argparse
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--local", action="store_true", help="Validate fixture/test mapping only")
    args = parser.parse_args()
    data = json.loads((ROOT / "crates/execution/tests/conditional_cleanup_evidence.json").read_text())
    assert data["issue"] == 100
    assert data["official_runner_commit"] == "cab9d1c3901e45c7705889c4f88284fdd93f4ae5"
    assert (ROOT / data["capture"]).is_file()
    tests = (ROOT / "crates/execution/tests/conditional_cleanup_test.rs").read_text()
    assert set(data["scenarios"]) == {f"S{i}" for i in range(1, 6)}
    covered = set()
    for scenario in data["scenarios"].values():
        assert f'async fn {scenario["test"]}(' in tests
        assert scenario["expected"]
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
