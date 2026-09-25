#!/usr/bin/env python3
"""Check issue #101 fixture parity and available live run evidence."""

import argparse
import json
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
EVIDENCE = ROOT / "crates/execution/tests/post_results_evidence.json"
OFFICIAL_PIN = "cab9d1c3901e45c7705889c4f88284fdd93f4ae5"
FILES = {
    "action.yml": "post_results_action.yml",
    "main.js": "post_results_main.js",
    "post.js": "post_results_post.js",
}


def check_local(data):
    assert data["issue"] == 101
    assert data["official_runner_commit"] == OFFICIAL_PIN
    assert (ROOT / data["capture"]).is_file()
    for live_name, test_name in FILES.items():
        live = ROOT / ".github/actions/post-results-probe" / live_name
        fixture = ROOT / "crates/execution/tests" / test_name
        assert live.read_bytes() == fixture.read_bytes(), f"fixture differs: {live_name}"
    workflow = (ROOT / ".github/workflows/post-results-live.yml").read_text()
    assert "post_failure:" in workflow and "cancel_cleanup:" in workflow
    print("post-results local evidence: fixture parity and workflow present")


def check_live(data):
    runs = data["live"]
    revisions = set()
    for lane in ("github_com_toolu_run_id", "github_com_official_run_id"):
        run_id = runs[lane]
        assert isinstance(run_id, int) and run_id > 0, f"{lane} is unverified"
        response = subprocess.run(
            ["gh", "api", f"repos/Falconiere/toolu-ghrunner/actions/runs/{run_id}"],
            check=True,
            capture_output=True,
            text=True,
        )
        try:
            run = json.loads(response.stdout)
        except json.JSONDecodeError as error:
            raise ValueError(
                f"{lane} returned invalid run JSON: {response.stdout[:500]!r}"
            ) from error
        assert run["event"] == "workflow_dispatch"
        assert run["path"].split("@", 1)[0] == ".github/workflows/post-results-live.yml"
        assert run["status"] == "completed", f"{lane} is still running"
        assert run["conclusion"] == "failure", f"{lane} did not fail as expected"
        revisions.add(run["head_sha"])
        print(f"{lane}: {run['html_url']}")
    assert len(revisions) == 1, "toolu and official runs used different revisions"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--local", action="store_true")
    parser.add_argument("--live", action="store_true")
    args = parser.parse_args()
    if not (args.local or args.live):
        parser.error("choose --local or --live")
    data = json.loads(EVIDENCE.read_text())
    check_local(data)
    if args.live:
        check_live(data)


if __name__ == "__main__":
    main()
