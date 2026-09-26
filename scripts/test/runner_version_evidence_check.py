#!/usr/bin/env python3
"""Verify issue #78's recorded GitHub.com runs against the actual GitHub API."""

import hashlib
import json
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
IDENTITY_SOURCES = {
    "crates/protocol/src/runner_version.rs",
    "crates/protocol/src/session.rs",
    "crates/protocol/src/lib.rs",
    "crates/wire/src/net/messages.rs",
    "crates/listener/src/handler.rs",
    "crates/listener/src/job_lifecycle.rs",
    "crates/listener/src/broker_message.rs",
}


def api(endpoint):
    """Read GitHub evidence, failing on absent credentials or inaccessible data."""
    return json.loads(subprocess.check_output(["gh", "api", endpoint], text=True))


def verify_source_hashes(evidence):
    """Bind the live evidence to every required identity source in this checkout."""
    assert set(evidence["source_sha256"]) == IDENTITY_SOURCES
    for path, expected in evidence["source_sha256"].items():
        assert hashlib.sha256((ROOT / path).read_bytes()).hexdigest() == expected, path


def main():
    """Require matching real successful jobs, exact identities and source hashes."""
    evidence = json.loads((ROOT / "docs/runner-version-evidence.json").read_text())
    repo = "Falconiere/toolu-ghrunner"
    assert evidence["compatibility_version"] == "2.337.0"
    assert evidence["official"]["binary_commit"] == "397b032cbf865e9c3ddfab89d533ec19325e1273"
    assert evidence["official"]["asset_sha256"] == (
        "9b1dc70626422526e3c94767cf024896beb15da5342a3f4819bf2feac13e0393"
    )
    for lane, runner in [("toolu", "toolu-78-linux"), ("official", "official-78-linux")]:
        entry = evidence[lane]
        run = api(f"repos/{repo}/actions/runs/{entry['run_id']}")
        assert run["status"] == "completed" and run["conclusion"] == "success"
        assert run["head_sha"] == evidence["workflow_sha"] == entry["workflow_sha"]
        assert run["path"] == ".github/workflows/noop-live.yml"
        assert run["html_url"] == entry["url"]
        jobs = api(f"repos/{repo}/actions/runs/{entry['run_id']}/jobs")["jobs"]
        assert len(jobs) == 1
        job = jobs[0]
        assert job["id"] == entry["job_id"]
        assert job["conclusion"] == "success" and job["status"] == "completed"
        assert job["runner_name"] == runner == entry["runner_name"]
        assert job["runner_id"] == entry["runner_id"]
        assert job["labels"] == entry["labels"] == ["self-hosted", "toolu-runner-v1"]
        assert any(step["name"] == "say hello" and step["conclusion"] == "success" for step in job["steps"])
    verify_source_hashes(evidence)
    policy = (ROOT / "docs/runner-updates.md").read_text()
    for required in ["2.337.0", "30 days", "critical security", "release maintainer", "operator", "revalidation", "GHES"]:
        assert required in policy, required
    assert evidence["ghes"] == "unverified: no endpoint or credentials supplied"
    print("Issue 78: real GitHub.com toolu/reference jobs and recorded source verified; GHES remains unverified")


if __name__ == "__main__":
    main()
