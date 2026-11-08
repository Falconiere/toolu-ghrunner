#!/usr/bin/env python3
"""Compare the toolu and official lanes of one compat-paired run (#236).

Usage:
  reference_compare.py RUN_ID --out PATH [--provenance FILE] [--capture FIXTURE]...
                        [--acknowledge FILE]
  reference_compare.py --verify PATH

The live mode reads the run through `gh`. It pairs jobs by variant, job, OS and
arch, normalizing only the lane token in job names (`probe-Linux-X64-toolu /
docker` and `probe-Linux-X64-official / docker` pair as `probe-Linux-X64 /
docker`), and it compares:

- job conclusions and the ordered (step name, conclusion) list per job;
- the `toJSON(needs)` observations each lane's consumer job uploaded, including
  JSON types;
- the exact payload artifact bytes;
- the expected outcome of each variant (`probe` green, `negative` red at the
  input assertions);
- the absence of probe-image containers on each Linux VM's daemon after the run,
  as recorded by that VM's host job;
- (#251) each VM's host observations (`compat-251-<platform>-host`), the lane's
  disposable value counted in every probe job log and annotation of that lane,
  the toolu-only lifecycle jobs, and the named assertions of compat_assertions.py
  that matrix rows cite as `report:<path>#<assertion>`.

Every report must cover EXPECTED_PLATFORMS, except the recorded single-host run
LEGACY_RUN_ID, whose rows name no platform and which stays unedited.

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
import re
import subprocess
import sys
import tempfile
from pathlib import Path

import compat_assertions
from compat_paired_host_report import disposable

ROOT = Path(__file__).resolve().parents[2]
REPO = "Falconiere/toolu-ghrunner"
LANES = ("toolu", "official")
VARIANTS = ("probe", "negative")
EXPECTED_PLATFORMS = (("Linux", "X64"), ("Linux", "ARM64"), ("macOS", "ARM64"))
# annotations/secrets/deploy/container are the #237 surface probes; the
# `surfaces` job of compat-paired.yml reads their GitHub records back. The rest
# of the second block is #251's (`extended` jobs run in the probe variant only).
LINUX_ONLY = ("docker", "container", "stack", "cancel-container", "docker-pre")
EXTENDED = ("leaks", "composite", "failure", "orphans", "timeout", "cancel", "cancel-container",
            "long", "pool-a", "pool-b", "docker-pre")
EXPECTED_JOBS = {"Linux": ("contexts", "docker", "services", "consumer",
                           "annotations", "secrets", "deploy", "container",
                           "stack") + EXTENDED,
                 "macOS": ("contexts", "services", "consumer", "annotations", "secrets", "deploy")
                 + tuple(j for j in EXTENDED if j not in LINUX_ONLY)}
# The 2.337.0 run in docs/compat-reference-103.json: one Linux host, no platform
# fields, and only the jobs the probe had before #237.
LEGACY_RUN_ID = 38301583727
LEGACY_JOBS = ("contexts", "docker", "services", "consumer")
RUN_FIELDS = ("id", "url", "head_sha", "workflow", "conclusion")
REQUIRED_LISTS = ("jobs", "unpaired", "payload_errors", "leftover_containers")
JOB_NAME = re.compile(r"^(?P<variant>[a-z]+)(?:-(?P<os>Linux|macOS)-(?P<arch>X64|ARM64))?"
                      r"-(?P<lane>toolu|official) / (?P<job>[^(]+?)(?: \(.*\))?$")
PROBE_IMAGE = "alpine@sha256:d9e853e87e55526f6b2917df91a2115c36dd7c696a35be12163d44e6e2a4b6bc"
# Expected failing steps per job for the empty-`who` variant; every other job
# and every `probe` job must succeed, except EXPECTATIONS.
NEGATIVE_FAILURES = {
    "contexts": "Assert typed inputs",
    "docker": "Verify container effects",
    "consumer": "Verify cross-job values",
}
# (variant, job) -> (job conclusion, failed step names) for the probe jobs that
# are meant to end badly. The cancel/timeout outcomes are the official lane's.
EXPECTATIONS = {
    # The missing job-container image fails setup; the always() step is still
    # attempted and fails without its container (official, run 42403405937).
    ("negative", "stack"): ("failure", ["Initialize containers", "Return workspace ownership"]),
    ("probe", "failure"): ("failure", ["Main failure"]),
    ("probe", "timeout"): ("failure", ["Step timeout"]),
    ("probe", "cancel"): ("cancelled", []),
    ("probe", "cancel-container"): ("cancelled", []),
}
LIFECYCLE_JOB = re.compile(r"^lifecycle-(?P<os>Linux|macOS)-(?P<arch>X64|ARM64)-toolu-(?P<job>[a-z0-9-]+)$")


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
    """(variant, lane, job, os, arch) from 'probe-Linux-X64-toolu / docker'; None if foreign."""
    match = JOB_NAME.match(name)
    return match and match.group("variant", "lane", "job", "os", "arch")


def same(left, right):
    """JSON equality: unlike Python's `==`, `true` is not `1` and `1` is not `1.0`."""
    return json.dumps(left, sort_keys=True) == json.dumps(right, sort_keys=True)


def scope(variant, os_name=None, arch=None):
    """`probe-Linux-X64`, or `probe` for a legacy row that names no platform."""
    return "-".join(part for part in (variant, os_name, arch) if part)


def row_scope(row):
    return scope(row["variant"], row.get("os"), row.get("arch"))


def forbidden_on(os_name, job):
    """True for a known job that `os_name` must not run (Docker on macOS)."""
    return job in EXPECTED_JOBS["Linux"] and job not in EXPECTED_JOBS.get(os_name, EXPECTED_JOBS["Linux"])


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
    if not same(toolu["conclusion"], official["conclusion"]):
        diff.append(f"conclusion {toolu['conclusion']} != {official['conclusion']}")
    if not same(toolu["steps"], official["steps"]):
        diff.append(f"steps {toolu['steps']} != {official['steps']}")
    return diff


def compare_jobs(jobs):
    """Pair jobs across lanes; returns (rows, unpaired). A duplicate key ends the script."""
    by_key = {}
    for job in jobs:
        parsed = split_job_name(job["name"])
        if parsed:
            variant, lane, name, os_name, arch = parsed
            lanes = by_key.setdefault((variant, name, os_name, arch), {})
            if lane in lanes:
                sys.exit(f"reference_compare: duplicate job {job['name']!r}")
            lanes[lane] = job_view(job)
    rows, unpaired = [], []
    for (variant, name, os_name, arch), lanes in sorted(by_key.items(), key=str):
        if forbidden_on(os_name, name) and all(v["conclusion"] == "skipped" for v in lanes.values()):
            continue
        if set(lanes) != set(LANES):
            unpaired.append(f"{scope(variant, os_name, arch)}/{name}: only {sorted(lanes)}")
            continue
        toolu, official = lanes["toolu"], lanes["official"]
        diff = lane_diff(toolu, official)
        rows.append({"variant": variant, "job": name, "os": os_name, "arch": arch,
                     "equal": not diff, "diff": diff, "toolu": toolu, "official": official})
    return rows, unpaired


def expected(variant, job):
    """(conclusion, failed steps) the official runner gives `job` in `variant`;
    the conclusion is None where only the failing step is pinned (the #103 rule)."""
    if (variant, job) in EXPECTATIONS:
        return EXPECTATIONS[(variant, job)]
    if variant == "negative" and job in NEGATIVE_FAILURES:
        return None, [NEGATIVE_FAILURES[job]]
    if variant == "negative" and job in EXTENDED:
        return "skipped", []
    return "success", []


def expectation_failures(rows):
    """(scope, job, message) for every job that missed its variant's expectation."""
    failures = []
    for row in rows:
        conclusion, failed_steps = expected(row["variant"], row["job"])
        for lane in LANES:
            view = row[lane]
            failed = [name for name, result in view["steps"] if result == "failure"]
            if conclusion is not None and view["conclusion"] != conclusion:
                failures.append((row_scope(row), row["job"], f"{lane} concluded {view['conclusion']}"))
            elif failed != failed_steps:
                failures.append((row_scope(row), row["job"], f"{lane} failed steps {failed}"))
    return failures


def download(run_id, name, dest):
    run_tool(["gh", "run", "download", str(run_id), "--repo", REPO, "-n", name, "-D", str(dest)])


def observations(run_id, tmp):
    """{scope: {lane: toJSON(needs)}} plus payload byte checks."""
    found, payload_errors = {}, []
    for os_name, arch in EXPECTED_PLATFORMS:
        for variant in VARIANTS:
            for lane in LANES:
                name = f"{os_name}-{arch}-{lane}-{variant}"
                dest = Path(tmp, name)
                download(run_id, f"compat-103-{name}-results", dest)
                found.setdefault(scope(variant, os_name, arch), {})[lane] = json.loads(
                    (dest / "results-103.json").read_text())
                payload = Path(tmp, f"{name}-payload")
                download(run_id, f"compat-103-{name}-payload", payload)
                data = (payload / "artifact-103.txt").read_bytes()
                if data != f"artifact-103-{run_id}\n".encode():
                    payload_errors.append(f"{name}: payload bytes {data!r}")
    return found, payload_errors


def probe_containers(listing, platform):
    """Probe-image lines of one VM's `docker ps -a --format '{{.ID}} {{.Image}}'`."""
    digest = PROBE_IMAGE.split("@", 1)[1]
    return [f"{platform}: {line}" for line in listing.splitlines()
            if digest in line or " alpine" in line]


def leftover_containers(run_id, tmp):
    """What each Linux host job saw on its VM. The VMs are gone by now, so a local
    `docker ps` would read an unrelated daemon; a missing artifact ends the script."""
    found = []
    for os_name, arch in EXPECTED_PLATFORMS:
        if "docker" in EXPECTED_JOBS[os_name]:
            platform = f"{os_name}-{arch}"
            dest = Path(tmp, f"{platform}-containers")
            download(run_id, f"compat-paired-{platform}-containers", dest)
            found += probe_containers((dest / "containers.txt").read_text(), platform)
    return found


def host_observations(run_id, tmp):
    """{platform: host-251.json} from each VM's `compat-251-<platform>-host` artifact."""
    found = {}
    for os_name, arch in EXPECTED_PLATFORMS:
        platform = f"{os_name}-{arch}"
        dest = Path(tmp, f"{platform}-host")
        download(run_id, f"compat-251-{platform}-host", dest)
        found[platform] = json.loads((dest / "host-251.json").read_text())
    return found


def lifecycle(jobs):
    """{platform: {job: conclusion}} of the toolu-only lifecycle jobs."""
    found = {}
    for job in jobs:
        match = LIFECYCLE_JOB.match(job["name"])
        if match:
            found.setdefault(f"{match['os']}-{match['arch']}", {})[match["job"]] = job["conclusion"]
    return found


def failure_category(stderr):
    """A bounded label for a failed `gh api` read: `http-<status>` or `error`.
    Never stderr itself, which can carry signed URLs or tokens."""
    match = re.search(r"\(HTTP (\d{3})\)", stderr or "")
    return f"http-{match.group(1)}" if match else "error"


def fetch(path, reasons=None, paginate=False):
    """`gh api path` text, or None when GitHub has nothing to return; the
    failure's category is counted in `reasons`. Job logs carry ANSI colour
    codes, which `gh` refuses to print without the flag. `paginate` follows
    every `Link: next` page (one JSON array per page); any failed page is None."""
    cmd = ["gh", "api", "--allow-escape-sequences"] + (["--paginate"] if paginate else []) + [path]
    out = subprocess.run(cmd, capture_output=True, text=True)
    if out.returncode == 0:
        return out.stdout
    if reasons is not None:
        category = failure_category(out.stderr)
        reasons[category] = reasons.get(category, 0) + 1
    return None


def leak_counts(run_id, jobs):
    """{platform: {lane: {logs, annotations, jobs_scanned, unreadable}}}: the
    lane's disposable value (job `leaks`) counted in every job log and check-run
    annotation of that lane. Counts only; nothing read is kept."""
    found = {}
    for job in jobs:
        parsed = split_job_name(job["name"])
        if not parsed or job["conclusion"] == "skipped" or not parsed[3]:
            continue
        _, lane, _, os_name, arch = parsed
        platform = f"{os_name}-{arch}"
        value = disposable(run_id, platform, lane)
        counts = found.setdefault(platform, {}).setdefault(
            lane, {"logs": 0, "annotations": 0, "jobs_scanned": 0, "unreadable": 0, "reasons": {}})
        log = fetch(f"repos/{REPO}/actions/jobs/{job['id']}/logs", counts["reasons"])
        notes = fetch(job["check_run_url"].split("api.github.com/", 1)[-1] + "/annotations?per_page=100",
                      counts["reasons"], paginate=True)
        if log is None or notes is None:
            counts["unreadable"] += 1
            continue
        counts["jobs_scanned"] += 1
        counts["logs"] += log.count(value)
        counts["annotations"] += notes.count(value)
    return found


def build_report(run_id, provenance, captures, acknowledged):
    run = gh_json("api", f"repos/{REPO}/actions/runs/{run_id}")
    # `--jq '.jobs[]' -c` prints one compact job object per line across pages.
    lines = run_tool(["gh", "api", "--paginate", f"repos/{REPO}/actions/runs/{run_id}/jobs?per_page=100",
                      "--jq", ".jobs[] | tojson"]).splitlines()
    jobs = [json.loads(line) for line in lines if line.strip()]
    rows, unpaired = compare_jobs(jobs)
    with tempfile.TemporaryDirectory() as tmp:
        found, payload_errors = observations(run_id, tmp)
        leftovers = leftover_containers(run_id, tmp)
        host = host_observations(run_id, tmp)
    results = {key: {"equal": same(lanes["toolu"], lanes["official"]), **lanes}
               for key, lanes in found.items()}
    report = {
        "issue": 230,
        "run": {"id": run_id, "url": run["html_url"], "head_sha": run["head_sha"],
                "workflow": run["path"], "conclusion": run["conclusion"]},
        "provenance": provenance,
        "jobs": rows,
        "unpaired": unpaired,
        "results": results,
        "payload_errors": payload_errors,
        "expectation_failures": [list(f) for f in expectation_failures(rows)],
        "leftover_containers": leftovers,
        "captures": {path: sha256(ROOT / path) for path in captures},
        "host": host,
        "leaks": leak_counts(run_id, jobs),
        "toolu_only": lifecycle(jobs),
    }
    report["acknowledged"] = pin_observations(report, acknowledged)
    report["assertions"] = assertions(report)
    return report


def assertions(report):
    """The named assertions recomputed from the stored data; {} for the legacy run."""
    if is_legacy(report):
        return {}
    # Fail closed: a malformed section or a missing observation scope fails
    # every assertion, whatever jobs it names.
    fatal = [m for key, job, m in shape_problems(report)]
    fatal += [f"unpaired job {u}" for u in report.get("unpaired") or []]
    fatal += [f"{field} is not an object" for field in ("results", "host", "leaks", "toolu_only")
              if not isinstance(report.get(field), dict)]
    fatal += [f"{key}: {m}" for key, job, m in coverage_problems(report) if job is None]
    return compat_assertions.evaluate(report, keyed_problems(report), fatal, EXPECTED_PLATFORMS,
                                      EXPECTED_JOBS)


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def is_legacy(report):
    """The recorded #103 report: its run id AND its issue. A current report that
    borrows the run id still needs every platform and every #251 section."""
    return (report.get("run") or {}).get("id") == LEGACY_RUN_ID and report.get("issue") == 103


def expected_platforms(report):
    return ((None, None),) if is_legacy(report) else EXPECTED_PLATFORMS


def well_formed(row):
    """A job row the comparison can read: variant and job names, and each lane a
    view with a conclusion and a step list."""
    return (isinstance(row, dict) and isinstance(row.get("variant"), str)
            and isinstance(row.get("job"), str)
            and all(isinstance(row.get(lane), dict) and isinstance(row[lane].get("steps"), list)
                    for lane in LANES))


def job_rows(report):
    """The report's well-formed job rows; shape_problems names the rest."""
    jobs = report.get("jobs")
    return [row for row in jobs if well_formed(row)] if isinstance(jobs, list) else []


def shape_problems(report):
    """Sections a report must carry. Job None: no acknowledgement can excuse these."""
    run = report.get("run") or {}
    problems = [(None, None, f"run lacks {field}") for field in RUN_FIELDS if not run.get(field)]
    if not report.get("provenance"):
        problems.append((None, None, "provenance is missing or empty"))
    problems += [(None, None, f"{field} is missing") for field in REQUIRED_LISTS
                 if not isinstance(report.get(field), list)]
    if isinstance(report.get("jobs"), list):
        bad = len(report["jobs"]) - len(job_rows(report))
        if bad:
            problems.append((None, None, f"{bad} malformed job row(s)"))
    return problems


def coverage_problems(report):
    """Every expected job and observation per platform, and nothing that must not exist."""
    rows, problems, seen = job_rows(report), [], set()
    platforms = expected_platforms(report)
    for row in rows:
        key, platform = (row_scope(row), row["job"]), (row.get("os"), row.get("arch"))
        if key in seen:
            problems.append((key[0], None, f"duplicate job {row['job']}"))
        seen.add(key)
        if platform not in platforms:
            problems.append((key[0], None, f"{row['job']} on unexpected platform {platform}"))
        elif forbidden_on(platform[0], row["job"]):
            problems.append((key[0], None, f"unexpected job {row['job']} on {platform[0]}"))
    results = report.get("results") or {}
    for os_name, arch in platforms:
        for variant in VARIANTS:
            key = scope(variant, os_name, arch)
            jobs = LEGACY_JOBS if os_name is None else EXPECTED_JOBS[os_name]
            problems += [(key, job, "missing job") for job in jobs if (key, job) not in seen]
            problems += [(key, None, f"{lane} observations missing or empty")
                         for lane in LANES if not (results.get(key) or {}).get(lane)]
    return problems


def keyed_problems(report):
    """(scope, job, message); recomputed from the stored lane views, never from
    the stored `equal` / `expectation_failures` flags, so editing a flag cannot
    hide a difference."""
    rows = job_rows(report)
    problems = shape_problems(report) + coverage_problems(report)
    for row in rows:
        diff = lane_diff(row["toolu"], row["official"])
        if diff:
            problems.append((row_scope(row), row["job"], f"differs: {diff}"))
    problems += expectation_failures(rows)
    problems += [(None, None, f"unpaired job {u}") for u in report.get("unpaired") or []]
    problems += [(key, None, "observations differ") for key, r in (report.get("results") or {}).items()
                 if not same(r.get("toolu"), r.get("official"))]
    problems += [(None, None, e) for e in report.get("payload_errors") or []]
    problems += [(None, None, f"leftover container {c}")
                 for c in report.get("leftover_containers") or []]
    return problems


def observed(report, key, job):
    """The recorded step lists of one job on both lanes in one scope, or None."""
    row = next((r for r in job_rows(report) if row_scope(r) == key and r["job"] == job), None)
    return None if row is None else {lane: row[lane]["steps"] for lane in LANES}


def covers(ack, report, key, job):
    """An acknowledgement covers a job only while both lanes still show exactly the
    step lists it pinned, so a new divergence in the same job is not excused."""
    steps = None if job is None else observed(report, key, job)
    return (steps is not None and ack["job"] == job and key in ack["variants"]
            and ack.get("observed", {}).get(key) == steps)


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
    for key, job, message in keyed_problems(report):
        match = next((i for i, a in enumerate(acks) if covers(a, report, key, job)), None)
        label = f"{key or '*'}/{job or '*'}: {message}"
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
    if not is_legacy(report):
        problems += [f"{field} is missing" for field in ("host", "leaks", "toolu_only", "assertions")
                     if not isinstance(report.get(field), dict)]
        problems += compat_assertions.stored_disagreements(report, assertions(report))
    for path, digest in report.get("captures", {}).items():
        if sha256(ROOT / path) != digest:
            problems.append(f"{path} changed since the run; re-record")
    return problems, notes


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("run_id", nargs="?", type=int)
    parser.add_argument("--out", type=Path, help="new report to write; required in live mode")
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
    if args.out is None:
        parser.error("--out is required in live mode")
    if args.out.exists():
        parser.error(f"{args.out} exists; recorded reports are never overwritten")
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
