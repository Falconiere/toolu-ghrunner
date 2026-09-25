#!/usr/bin/env python3
"""Verify issue 70's sanitized acquisition and recorded live runs."""

import argparse
import base64
import hashlib
import json
from pathlib import Path
import re
import subprocess
from uuid import NAMESPACE_URL, uuid5


ROOT = Path(__file__).resolve().parents[2]
EVIDENCE = ROOT / "crates/execution/tests/job_outputs_evidence.json"
REPO = "Falconiere/toolu-ghrunner"
WORKFLOW = ROOT / ".github/workflows/job-outputs-70.yml"


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def api(path):
    return json.loads(subprocess.check_output(["gh", "api", f"repos/{REPO}/{path}"]))


def mapping(token):
    assert token["type"] == 2, "jobOutputs must be a mapping token"
    entries = {}
    for entry in token["map"]:
        key = entry["Key"]
        assert key["type"] == 0, "output key must be a literal token"
        entries[key["lit"]] = entry["Value"]
    return entries


def check_capture(evidence):
    record = evidence["capture"]
    path = ROOT / record["fixture"]
    assert digest(path) == record["fixture_sha256"], "fixture hash mismatch"
    job = json.loads(path.read_text())
    assert job["jobId"] == record["job_id"], "captured job ID mismatch"
    github = {item["k"]: item["v"] for item in job["contextData"]["github"]["d"]}
    assert str(github["run_id"]) == str(record["run_id"]), "captured run ID mismatch"
    assert github["sha"] == record["revision"], "captured revision mismatch"
    assert len(job["steps"]) == 1, "capture must have one real step"
    step = job["steps"][0]
    assert (step["id"], step["contextName"]) == (
        record["step_id"], record["step_context_name"]
    ), "captured step identity mismatch"
    assert record["step_id"] != record["step_context_name"], "step IDs must differ"
    outputs = mapping(job["jobOutputs"])
    assert len(outputs) == 1, "capture must have one output entry"
    assert outputs["value"] == {
        "col": 14,
        "expr": "steps.produce.outputs.value",
        "file": 1,
        "line": 16,
        "type": 3,
    }, "captured output expression differs"

    fields = {}
    for name, item in job["variables"].items():
        if item.get("isSecret"):
            fields[f"variables/{name}/value"] = item["value"]
    for index, item in enumerate(job["mask"]):
        if item.get("value"):
            fields[f"mask/{index}/value"] = item["value"]
    for index, endpoint in enumerate(job["resources"]["endpoints"]):
        for name, value in endpoint.get("authorization", {}).get("parameters", {}).items():
            fields[f"resources/endpoints/{index}/authorization/parameters/{name}"] = value
    assert len(fields) == record["secret_fields_replaced"], "secret field count mismatch"
    for field, actual in fields.items():
        expected = str(uuid5(NAMESPACE_URL, f"toolu-ghrunner/70/job_outputs_message.json/{field}"))
        assert actual == expected, f"{field}: expected synthetic placeholder"
    serialized = path.read_text()
    assert not re.search(r"gh[psu]_[A-Za-z0-9_]{16,}|github_pat_[A-Za-z0-9_]{16,}", serialized), (
        "token-like value in fixture"
    )
    assert len(evidence["capture_workflow_sha256"]) == 64, "capture workflow digest missing"
    assert evidence["reference_source"] == "v2.337.0", "reference version changed"
    print("Captured jobOutputs, step identity, provenance, and synthetic credentials verified.")


def check_live(evidence):
    captured_workflow = api(
        f"contents/.github/workflows/job-outputs-70.yml?ref={evidence['capture']['revision']}"
    )
    captured_bytes = base64.b64decode(captured_workflow["content"])
    assert hashlib.sha256(captured_bytes).hexdigest() == evidence["capture_workflow_sha256"], (
        "remote capture workflow digest mismatch"
    )
    assert digest(WORKFLOW) == evidence["workflow_sha256"], "live workflow digest mismatch"
    for name, expected in evidence["action_sha256"].items():
        assert digest(ROOT / name) == expected, f"{name}: local action digest mismatch"
    for lane in ("toolu", "reference"):
        record = evidence["runs"][lane]
        assert record is not None, f"{lane} live run is unverified"
        run = api(f"actions/runs/{record['id']}")
        assert run["head_sha"] == record["revision"], f"{lane}: run revision mismatch"
        assert (run["status"], run["conclusion"]) == ("completed", "success"), (
            f"{lane}: run did not succeed"
        )
        jobs = api(f"actions/runs/{record['id']}/jobs?per_page=100")["jobs"]
        by_name = {job["name"]: job for job in jobs}
        for name in (record["producer"], record["consumer"]):
            assert by_name[name]["conclusion"] == "success", f"{lane}/{name}: job did not succeed"
        assert by_name[record["producer"]]["runner_name"] == record["runner"], (
            f"{lane}: wrong producer runner"
        )
        for name, expected in {
            ".github/workflows/job-outputs-70.yml": evidence["workflow_sha256"],
            **evidence["action_sha256"],
        }.items():
            remote = api(f"contents/{name}?ref={record['revision']}")
            actual = hashlib.sha256(base64.b64decode(remote["content"])).hexdigest()
            assert actual == expected, f"{lane}/{name}: remote digest mismatch"
        print(f"{lane}: {run['html_url']} producer and downstream consumer passed.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--capture", action="store_true")
    group.add_argument("--live", action="store_true")
    args = parser.parse_args()
    evidence = json.loads(EVIDENCE.read_text())
    check_capture(evidence)
    if args.live:
        check_live(evidence)


if __name__ == "__main__":
    main()
