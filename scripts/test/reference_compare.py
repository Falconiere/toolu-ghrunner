#!/usr/bin/env python3
"""Compare the toolu and official lanes of one compat-reference-103 run (#103).

Usage:
  reference_compare.py RUN_ID [--out PATH] [--provenance FILE] [--capture FIXTURE]...
                        [--acknowledge FILE]
  reference_compare.py --verify PATH

The live mode reads the run through `gh` and the local Docker daemon. It
normalizes only the lane token in job names (`probe-toolu / docker` and
`probe-official / docker` become `probe / docker`), and it compares:

- job conclusions and the ordered (step name, conclusion) list per job;
- the `toJSON(needs)` observations each lane's consumer job uploaded;
- the exact payload artifact bytes;
- the expected outcome of each variant (`probe` green, `negative` red at the
  input assertions);
- the absence of probe-image containers on the daemon after the run.

Step names, conclusions and observation values are never normalized: any
difference there is a semantic difference and is reported as one. A
difference that has been diagnosed but deliberately not fixed is recorded under
`acknowledged` (diagnosis, upstream reference, affected matrix rows): it stays
in the report and keeps those matrix rows unverified, but no longer fails
`--verify`. An acknowledgement that matches no observed difference is itself a
failure, so a fixed gap cannot keep a stale excuse. `--verify` re-validates a
recorded report offline and re-hashes the committed captures.
"""

import argparse
import hashlib
import json
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
REPO = "Falconiere/toolu-ghrunner"
WORKFLOW = "compat-reference-103.yml"
LANES = ("toolu", "official")
PROBE_IMAGE = "alpine@sha256:d9e853e87e55526f6b2917df91a2115c36dd7c696a35be12163d44e6e2a4b6bc"
# Expected failing steps per job for the empty-`who` variant; every other job
# and every `probe` job must succeed.
NEGATIVE_FAILURES = {
    "contexts": "Assert typed inputs",
    "docker": "Verify container effects",
    "consumer": "Verify cross-job values",
}


def run_tool(args):
    """stdout of `args`; a missing tool or a failed call ends the script with its stderr."""
    try:
        return subprocess.run(args, check=True, capture_output=True, text=True).stdout
    except FileNotFoundError:
        sys.exit(f"reference_compare: {args[0]} is not on PATH")
    except subprocess.CalledProcessError as error:
        sys.exit(f"reference_compare: {' '.join(args[:3])} failed: {error.stderr.strip()}")


def gh_json(*args):
    return json.loads(run_tool(["gh", *args]))


def split_job_name(name):
    """('probe', 'toolu', 'docker') from 'probe-toolu / docker'; None if foreign."""
    caller, sep, job = name.partition(" / ")
    variant, dash, lane = caller.rpartition("-")
    if not sep or not dash or lane not in LANES:
        return None
    return variant, lane, job.split(" (")[0]


def job_view(job):
    return {
        "conclusion": job["conclusion"],
        "steps": [[s["name"], s["conclusion"]] for s in job.get("steps", [])],
        "runner_name": job.get("runner_name"),
        "url": job["html_url"],
    }


def lane_diff(toolu, official):
    """Every difference between two lanes' views of one job."""
    diff = []
    if toolu["conclusion"] != official["conclusion"]:
        diff.append(f"conclusion {toolu['conclusion']} != {official['conclusion']}")
    if toolu["steps"] != official["steps"]:
        diff.append(f"steps {toolu['steps']} != {official['steps']}")
    return diff


def compare_jobs(jobs):
    """Pair jobs across lanes; returns (rows, unpaired)."""
    by_key = {}
    for job in jobs:
        parsed = split_job_name(job["name"])
        if parsed:
            variant, lane, name = parsed
            by_key.setdefault((variant, name), {})[lane] = job_view(job)
    rows, unpaired = [], []
    for (variant, name), lanes in sorted(by_key.items()):
        if set(lanes) != set(LANES):
            unpaired.append(f"{variant}/{name}: only {sorted(lanes)}")
            continue
        toolu, official = lanes["toolu"], lanes["official"]
        diff = lane_diff(toolu, official)
        rows.append({"variant": variant, "job": name, "equal": not diff, "diff": diff,
                     "toolu": toolu, "official": official})
    return rows, unpaired


def expectation_failures(rows):
    """(variant, job, message) for every job that missed its variant's expectation."""
    failures = []
    for row in rows:
        for lane in LANES:
            view = row[lane]
            if row["variant"] == "probe" or row["job"] not in NEGATIVE_FAILURES:
                if view["conclusion"] != "success":
                    failures.append((row["variant"], row["job"],
                                     f"{lane} concluded {view['conclusion']}"))
                continue
            failed = [name for name, conclusion in view["steps"] if conclusion == "failure"]
            if failed != [NEGATIVE_FAILURES[row["job"]]]:
                failures.append(("negative", row["job"], f"{lane} failed steps {failed}"))
    return failures


def download(run_id, name, dest):
    run_tool(["gh", "run", "download", str(run_id), "--repo", REPO, "-n", name, "-D", str(dest)])


def observations(run_id, tmp):
    """{variant: {lane: toJSON(needs)}} plus payload byte checks."""
    found, payload_errors = {}, []
    for variant in ("probe", "negative"):
        for lane in LANES:
            dest = Path(tmp, f"{lane}-{variant}")
            download(run_id, f"compat-103-{lane}-{variant}-results", dest)
            found.setdefault(variant, {})[lane] = json.loads((dest / "results-103.json").read_text())
            payload = Path(tmp, f"{lane}-{variant}-payload")
            download(run_id, f"compat-103-{lane}-{variant}-payload", payload)
            data = (payload / "artifact-103.txt").read_bytes()
            if data != f"artifact-103-{run_id}\n".encode():
                payload_errors.append(f"{lane}/{variant}: payload bytes {data!r}")
    return found, payload_errors


def leftover_containers():
    out = run_tool(["docker", "ps", "-a", "--format", "{{.ID}} {{.Image}}"])
    digest = PROBE_IMAGE.split("@", 1)[1]
    return [line for line in out.splitlines() if digest in line or " alpine" in line]


def build_report(run_id, provenance, captures, acknowledged):
    run = gh_json("api", f"repos/{REPO}/actions/runs/{run_id}")
    # `--jq '.jobs[]' -c` prints one compact job object per line across pages.
    lines = run_tool(["gh", "api", "--paginate", f"repos/{REPO}/actions/runs/{run_id}/jobs?per_page=100",
                      "--jq", ".jobs[] | tojson"]).splitlines()
    jobs = [json.loads(line) for line in lines if line.strip()]
    rows, unpaired = compare_jobs(jobs)
    with tempfile.TemporaryDirectory() as tmp:
        found, payload_errors = observations(run_id, tmp)
    results = {variant: {"equal": lanes["toolu"] == lanes["official"], **lanes}
               for variant, lanes in found.items()}
    report = {
        "issue": 103,
        "run": {"id": run_id, "url": run["html_url"], "head_sha": run["head_sha"],
                "workflow": run["path"], "conclusion": run["conclusion"]},
        "provenance": provenance,
        "jobs": rows,
        "unpaired": unpaired,
        "results": results,
        "payload_errors": payload_errors,
        "expectation_failures": [list(f) for f in expectation_failures(rows)],
        "leftover_containers": leftover_containers(),
        "captures": {path: sha256(ROOT / path) for path in captures},
    }
    report["acknowledged"] = pin_observations(report, acknowledged)
    return report


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def keyed_problems(report):
    """(variant, job, message); recomputed from the stored lane views, never from
    the stored `equal` / `expectation_failures` flags, so editing a flag cannot
    hide a difference."""
    problems = []
    for row in report["jobs"]:
        diff = lane_diff(row["toolu"], row["official"])
        if diff:
            problems.append((row["variant"], row["job"], f"differs: {diff}"))
    problems += [(v, j, msg) for v, j, msg in expectation_failures(report["jobs"])]
    problems += [(None, None, f"unpaired job {u}") for u in report["unpaired"]]
    problems += [(v, None, "observations differ") for v, r in report["results"].items()
                 if r.get("toolu") != r.get("official")]
    problems += [(None, None, e) for e in report["payload_errors"]]
    problems += [(None, None, f"leftover container {c}") for c in report["leftover_containers"]]
    expected_jobs = {(v, j) for v in ("probe", "negative")
                     for j in ("contexts", "docker", "services", "consumer")}
    seen = {(r["variant"], r["job"]) for r in report["jobs"]}
    problems += [(v, j, "missing job") for v, j in sorted(expected_jobs - seen)]
    return problems


def observed(report, variant, job):
    """The recorded step lists of one job on both lanes, or None."""
    row = next((r for r in report["jobs"] if r["variant"] == variant and r["job"] == job), None)
    return None if row is None else {lane: row[lane]["steps"] for lane in LANES}


def covers(ack, report, variant, job):
    """An acknowledgement covers a job only while both lanes still show exactly the
    step lists it pinned, so a new divergence in the same job is not excused."""
    return (job is not None and ack["job"] == job and variant in ack["variants"]
            and ack.get("observed", {}).get(variant) == observed(report, variant, job))


def pin_observations(report, acknowledged):
    """Record, per acknowledged job, the exact step lists the acknowledgement excuses."""
    for ack in acknowledged:
        ack["observed"] = {v: observed(report, v, ack["job"]) for v in ack["variants"]}
    return acknowledged


def verify(report):
    """(problems, acknowledged_notes) for a recorded report."""
    problems, notes = [], []
    acks = report.get("acknowledged", [])
    used = set()
    for variant, job, message in keyed_problems(report):
        match = next((i for i, a in enumerate(acks) if covers(a, report, variant, job)), None)
        label = f"{variant or '*'}/{job or '*'}: {message}"
        if match is None:
            problems.append(label)
        else:
            used.add(match)
            notes.append(f"acknowledged [{acks[match]['finding']}] {label}")
    for index, ack in enumerate(acks):
        if not ack.get("diagnosis") or not ack.get("matrix_rows"):
            problems.append(f"acknowledgement {ack.get('finding')} lacks diagnosis/matrix_rows")
        if index not in used:
            problems.append(f"stale acknowledgement {ack.get('finding')}: no matching difference")
    if not report.get("captures"):
        problems.append("no capture recorded")
    for path, digest in report.get("captures", {}).items():
        if sha256(ROOT / path) != digest:
            problems.append(f"{path} changed since the run; re-record")
    return problems, notes


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("run_id", nargs="?", type=int)
    parser.add_argument("--out", type=Path, default=ROOT / "docs/compat-reference-103.json")
    parser.add_argument("--provenance", type=Path, help="JSON with runner/image provenance")
    parser.add_argument("--capture", action="append", default=[],
                        help="repo-relative sanitized capture fixture to pin (repeatable)")
    parser.add_argument("--acknowledge", type=Path,
                        help="JSON list of diagnosed, deliberately unfixed differences")
    parser.add_argument("--verify", type=Path, metavar="PATH")
    args = parser.parse_args(argv)
    if args.verify:
        problems, notes = verify(json.loads(args.verify.read_text()))
        for note in notes:
            print(f"{args.verify}: {note}")
        for problem in problems:
            print(f"{args.verify}: {problem}", file=sys.stderr)
        if not problems:
            print(f"{args.verify}: no unacknowledged difference; {len(notes)} acknowledged")
        return 1 if problems else 0
    if args.run_id is None:
        parser.error("RUN_ID is required unless --verify is given")
    provenance = json.loads(args.provenance.read_text()) if args.provenance else {}
    acknowledged = json.loads(args.acknowledge.read_text()) if args.acknowledge else []
    report = build_report(args.run_id, provenance, args.capture, acknowledged)
    args.out.write_text(json.dumps(report, indent=2) + "\n")
    problems, notes = verify(report)
    for note in notes:
        print(note)
    for problem in problems:
        print(problem, file=sys.stderr)
    print(f"wrote {args.out}: {len(report['jobs'])} paired jobs, {len(problems)} problems")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
