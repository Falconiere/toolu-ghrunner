#!/usr/bin/env python3
"""Verify issue 72's captured-job provenance and recorded live process probes."""

import argparse
import base64
import hashlib
import json
from pathlib import Path
import subprocess


ROOT = Path(__file__).resolve().parents[2]
REPO = "Falconiere/toolu-ghrunner"
SOURCE_PIN = "cab9d1c3901e45c7705889c4f88284fdd93f4ae5"
LANES = (
    "reference-host-macos",
    "reference-host-linux",
    "reference-container-linux",
    "toolu-host-macos",
    "toolu-container-linux",
)
HOST_STEPS = {
    "Install committed probe actions",
    "Shell keeps job CI and forces GitHub Actions",
    "Shell keeps false step CI",
    "Shell keeps empty step CI",
    "Node pre main post",
    "Nested composite shells",
    "Nested Node pre main post",
    "Nested Node inherits parent step CI",
    "Later job env does not replace Node post step env",
}
CONTAINER_STEPS = {
    "Install committed probe actions",
    "Shell keeps container CI and forces GitHub Actions",
    "Node pre main post in container",
    "Nested composite shells in container",
    "Nested Node pre main post in container",
    "Nested Node inherits parent step CI in container",
    "Later job env does not replace Node post step env",
}
JOB_NAMES = {
    "reference-host-macos": "reference-host-macos-15",
    "reference-host-linux": "reference-host-ubuntu-24.04",
    "reference-container-linux": "reference-container-ubuntu-24.04",
    "toolu-host-macos": "toolu-host-macos",
    "toolu-container-linux": "toolu-container-linux",
}


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def api(path):
    return json.loads(subprocess.check_output(["gh", "api", f"repos/{REPO}/{path}"]))


def check_local(evidence):
    capture = evidence["capture"]
    path = ROOT / capture["path"]
    provenance = json.loads(
        (ROOT / "crates/execution/tests/incoming_contexts_evidence.json").read_text()
    )["captures"][path.name]
    assert digest(path) == capture["sha256"] == provenance["sha256"]
    message = json.loads(path.read_text())
    assert message["jobId"] == capture["job_id"] == provenance["job_id"]
    assert capture["run_id"] == provenance["run_id"]
    action = [step for step in message["steps"] if step.get("contextName") == "__self"]
    assert len(action) == 1 and action[0]["id"] == capture["action_step_id"]
    container = ROOT / evidence["container_capture"]["path"]
    assert digest(container) == evidence["container_capture"]["sha256"]
    container_message = json.loads(container.read_text())
    assert container_message["jobId"] == evidence["container_capture"]["job_id"]
    assert container_message["jobContainer"]["type"] == 2
    assert evidence["reference_source"] == SOURCE_PIN
    for relative, expected in evidence["files_sha256"].items():
        assert digest(ROOT / relative) == expected, f"{relative}: content hash changed"
    print("Captured acquisitions, wire IDs, and committed probe hashes verified.")


def check_run(lane, record, evidence):
    run = api(f"actions/runs/{record['id']}")
    assert run["head_sha"] == record["revision"], f"{lane}: revision mismatch"
    assert run["status"] == "completed" and run["conclusion"] == "success", (
        f"{lane}: {run['status']}/{run['conclusion']}"
    )
    jobs = api(f"actions/runs/{record['id']}/jobs?per_page=100")["jobs"]
    matching = [job for job in jobs if job["name"] == JOB_NAMES[lane]]
    assert len(matching) == 1, f"{lane}: expected one matching job"
    job = matching[0]
    assert job["conclusion"] == "success", f"{lane}: job failed"
    if record.get("runner"):
        assert job["runner_name"] == record["runner"], f"{lane}: runner identity changed"
    passed = {step["name"] for step in job["steps"] if step["conclusion"] == "success"}
    required = CONTAINER_STEPS if "container" in lane else HOST_STEPS
    assert required <= passed, f"{lane}: missing passing steps {required - passed}"
    for relative, expected in evidence["files_sha256"].items():
        remote = api(f"contents/{relative}?ref={record['revision']}")
        assert hashlib.sha256(base64.b64decode(remote["content"])).hexdigest() == expected, (
            f"{lane}: remote {relative} differs"
        )
    print(f"{lane}: {run['html_url']} passed the committed process assertions.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence", type=Path)
    parser.add_argument("--live", action="store_true", help="Check recorded runs with GitHub")
    parser.add_argument("--require-live", action="store_true", help="Fail for any absent live lane")
    args = parser.parse_args()
    evidence = json.loads(args.evidence.read_text())
    check_local(evidence)
    assert set(evidence["runs"]) == set(LANES), "run lane set changed"
    revisions = {record["revision"] for record in evidence["runs"].values() if record}
    assert len(revisions) <= 1, "live lanes must use the same committed revision"
    for lane in LANES:
        record = evidence["runs"][lane]
        if record is None:
            reason = evidence["unverified"].get(lane)
            assert reason, f"{lane}: missing unverified reason"
            if args.require_live:
                parser.error(f"{lane}: unverified — {reason}")
            print(f"{lane}: unverified — {reason}")
        elif args.live or args.require_live:
            check_run(lane, record, evidence)


if __name__ == "__main__":
    main()
