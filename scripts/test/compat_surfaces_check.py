#!/usr/bin/env python3
"""Read the #237 surfaces of one compat-paired run back from the GitHub API.

The probe jobs (.github/workflows/compat-reference-103-probe.yml) leave records
only GitHub holds: check-run annotations and deployment statuses. This script
fetches them for every lane with the run's own token, keeps a sanitized capture
(`--captures`), and evaluates each scope (`<variant>-<platform>-<lane>`):

- `annotations`: exactly the three workflow-command annotations, none from the
  control step. The endpoint is the job's own `check_run_url`, never an assumed
  job-id = check-run-id identity.
- `secrets`: the upstream skip-output and skip-environment-URL warnings, and an
  empty `environment_url` on its deployment status.
- `deploy`: the safe URL on its deployment status (matched by the status
  `log_url`, which names the job).
- job conclusions for the in-job checks: OIDC in `services`, outputs and
  artifact digests in `consumer`, the job-container upload in `container`
  (toolu and official Linux lanes; the accelerated lane's runner itself runs in
  a container, which has no Docker daemon).

Only toolu lanes (`toolu`, `toolu-accelerated`) fail the check; official lanes
are recorded for #251. `--verify <capture>` re-evaluates a committed capture
offline. No token, claim value or signed URL may appear in a capture.
"""

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

REPO = "Falconiere/toolu-ghrunner"
API = "https://api.github.com/"
PLATFORMS = ("Linux-X64", "Linux-ARM64", "macOS-ARM64")
TOOLU_LANES = ("toolu", "toolu-accelerated")
SAFE_ENV, SECRET_ENV = "compat-237-safe", "compat-237-secret"
JOB_NAME = re.compile(r"^(?P<variant>probe|negative)-(?P<platform>(?:Linux|macOS)-(?:X64|ARM64))"
                      r"-(?P<lane>toolu-accelerated|toolu|official) / (?P<job>[^(]+?)(?: \(.*\))?$")
ANNOTATED = "compat-237/annotated.txt"
EXPECTED_ANNOTATIONS = sorted([
    (ANNOTATED, 3, 3, 2, 9, "failure", "compat-237 error", "compat-237 error marker"),
    (ANNOTATED, 5, 5, None, None, "warning", "compat-237 warning", "compat-237 warning marker"),
    (ANNOTATED, 7, 7, None, None, "notice", "compat-237 notice", "compat-237 notice marker"),
], key=str)
SECRET_WARNINGS = sorted([
    "Skip output 'masked' since it may contain secret.",
    f"Skip setting environment url as environment '{SECRET_ENV}' may contain secret.",
])
# A JWT (header.payload.) or a signed blob URL must never be captured.
LEAKS = re.compile(r"eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.|[?&]sig=")


def expected_scopes():
    scopes = [f"{variant}-{platform}-{lane}" for variant in ("probe", "negative")
              for platform in PLATFORMS for lane in ("toolu", "official")]
    return scopes + ["probe-Linux-X64-toolu-accelerated"]


def gh_lines(*args):
    try:
        return subprocess.run(["gh", "api", "--paginate", *args], check=True,
                              capture_output=True, text=True).stdout.splitlines()
    except subprocess.CalledProcessError as error:
        sys.exit(f"compat_surfaces_check: gh api {args[0]} failed: {error.stderr.strip()}")


def gh_items(path):
    return [json.loads(line) for line in gh_lines(path, "--jq", ".[] | tojson")]


def fetch(run_id, attempt):
    """The sanitized capture: jobs, their annotations, and both environments' deployments."""
    run = json.loads("".join(gh_lines(f"repos/{REPO}/actions/runs/{run_id}")))
    jobs = [json.loads(line) for line in gh_lines(
        f"repos/{REPO}/actions/runs/{run_id}/attempts/{attempt}/jobs?per_page=100",
        "--jq", ".jobs[] | tojson")]
    capture = {
        "run": {"id": run["id"], "attempt": attempt, "head_sha": run["head_sha"],
                "head_branch": run["head_branch"], "url": run["html_url"], "path": run["path"]},
        "jobs": [{"id": j["id"], "name": j["name"], "conclusion": j["conclusion"],
                  "html_url": j["html_url"], "check_run_url": j["check_run_url"],
                  "runner_name": j.get("runner_name"),
                  "steps": [{"name": s["name"], "conclusion": s["conclusion"]}
                            for s in j.get("steps", [])]} for j in jobs],
        "annotations": {}, "deployments": {},
    }
    for job in capture["jobs"]:
        parsed = JOB_NAME.match(job["name"])
        if parsed and parsed["job"] in ("annotations", "secrets"):
            items = gh_items(job["check_run_url"].removeprefix(API) + "/annotations?per_page=100")
            capture["annotations"][str(job["id"])] = [
                {k: a.get(k) for k in ("path", "start_line", "end_line", "start_column",
                                       "end_column", "annotation_level", "title", "message")}
                for a in items]
    for env in (SAFE_ENV, SECRET_ENV):
        capture["deployments"][env] = [
            {"id": d["id"], "statuses": [
                {k: s.get(k) for k in ("state", "environment_url", "log_url")}
                for s in gh_items(f"repos/{REPO}/deployments/{d['id']}/statuses?per_page=100")]}
            for d in gh_items(f"repos/{REPO}/deployments?environment={env}"
                              f"&sha={run['head_sha']}&per_page=100")]
    return capture


def leaks(capture):
    return LEAKS.findall(json.dumps(capture))


def annotation_key(a):
    return (a["path"], a["start_line"], a["end_line"], a["start_column"], a["end_column"],
            a["annotation_level"], a["title"], a["message"])


def deployed_url(capture, env, job):
    """environment_url of the success status that names `job`; None when absent."""
    for deployment in capture["deployments"].get(env, []):
        for status in deployment["statuses"]:
            if status["state"] == "success" and (status["log_url"] or "").endswith(f"/job/{job['id']}"):
                return status["environment_url"]
    return None


def step(job, name):
    return next((s["conclusion"] for s in job["steps"] if s["name"] == name), None)


SCOPE = re.compile(r"^(?P<variant>probe|negative)-(?P<lane_input>(?P<os>Linux|macOS)-(?:X64|ARM64)"
                   r"-(?P<lane>.+))$")


def check_jobs(scope, jobs):
    """(seen, problems) for the jobs a scope must run and their in-job checks."""
    parsed = SCOPE.match(scope)
    wanted = ["annotations", "secrets", "deploy", "services", "consumer"]
    # The accelerated lane's runner runs in a container with no Docker daemon.
    wanted += ["container"] if parsed["os"] == "Linux" and parsed["lane"] != "toolu-accelerated" else []
    seen, problems = {}, []
    for name in wanted:
        job = jobs.get(name)
        if job is None:
            problems.append(f"{name}: job missing")
            continue
        seen[name] = {"conclusion": job["conclusion"],
                      "steps": {s["name"]: s["conclusion"] for s in job["steps"]}}
        # The negative consumer fails by design at "Verify cross-job values".
        if (name != "consumer" or parsed["variant"] == "probe") and job["conclusion"] != "success":
            problems.append(f"{name}: concluded {job['conclusion']}")
    oidc = "OIDC allowed" if parsed["variant"] == "probe" else "OIDC denied without id-token permission"
    if "services" in jobs and step(jobs["services"], oidc) != "success":
        problems.append(f"services: step {oidc!r} concluded {step(jobs['services'], oidc)}")
    if "consumer" in jobs and step(jobs["consumer"], "Verify outputs and artifact digests") != "success":
        problems.append("consumer: step 'Verify outputs and artifact digests' did not succeed")
    return seen, problems


def check_annotations(jobs, capture, observed):
    problems = []
    if "annotations" in jobs:
        found = sorted((annotation_key(a) for a in
                        capture["annotations"].get(str(jobs["annotations"]["id"]), [])), key=str)
        observed["annotations"] = [list(a) for a in found]
        if found != EXPECTED_ANNOTATIONS:
            problems.append(f"annotations: {found} != {EXPECTED_ANNOTATIONS}")
        if any("control" in (a[7] or "") for a in found):
            problems.append("annotations: the control step produced an annotation")
    if "secrets" in jobs:
        warnings = sorted(a["message"] for a in capture["annotations"].get(str(jobs["secrets"]["id"]), [])
                          if a["annotation_level"] == "warning")
        observed["secret_warnings"] = warnings
        if warnings != SECRET_WARNINGS:
            problems.append(f"secrets: warnings {warnings} != {SECRET_WARNINGS}")
    return problems


def check_deployments(scope, jobs, capture, observed):
    problems = []
    if "secrets" in jobs:
        observed["secret_url"] = deployed_url(capture, SECRET_ENV, jobs["secrets"])
        if observed["secret_url"] != "":
            problems.append(f"secrets: deployment environment_url {observed['secret_url']!r}, wanted ''")
    if "deploy" in jobs:
        parsed = SCOPE.match(scope)
        want = (f"https://example.com/compat-237/{parsed['lane_input']}-{parsed['variant']}/"
                f"{capture['run']['id']}")
        observed["deploy_url"] = deployed_url(capture, SAFE_ENV, jobs["deploy"])
        if observed["deploy_url"] != want:
            problems.append(f"deploy: deployment environment_url {observed['deploy_url']!r}, wanted {want!r}")
    return problems


def evaluate_scope(scope, jobs, capture):
    """Observations and problems of one `<variant>-<platform>-<lane>` scope."""
    seen, problems = check_jobs(scope, jobs)
    observed = {"jobs": seen}
    problems += check_annotations(jobs, capture, observed)
    problems += check_deployments(scope, jobs, capture, observed)
    observed["problems"] = problems
    return observed


def evaluate(capture):
    """(report, failures): failures are toolu-lane problems, missing scopes and leaks."""
    by_scope = {}
    for job in capture["jobs"]:
        parsed = JOB_NAME.match(job["name"])
        if parsed:
            scope = f"{parsed['variant']}-{parsed['platform']}-{parsed['lane']}"
            by_scope.setdefault(scope, {})[parsed["job"].strip()] = job
    guard = next((j["conclusion"] for j in capture["jobs"] if j["name"] == "guard"), None)
    report = {"run": capture["run"], "guard": guard,
              "scopes": {s: evaluate_scope(s, by_scope.get(s, {}), capture) for s in expected_scopes()}}
    failures = [f"leak in capture: {m[:12]}…" for m in leaks(capture)]
    if guard != "success":
        failures.append(f"guard concluded {guard}")
    for scope, observed in report["scopes"].items():
        if SCOPE.match(scope)["lane"] in TOOLU_LANES:
            failures += [f"{scope}: {p}" for p in observed["problems"]]
    return report, failures


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--run", type=int, help="compat-paired run id to fetch")
    parser.add_argument("--attempt", type=int, default=1)
    parser.add_argument("--out", help="report path (with --run)")
    parser.add_argument("--captures", help="directory for the sanitized capture (with --run)")
    parser.add_argument("--verify", help="re-evaluate a committed capture offline")
    args = parser.parse_args()
    if args.verify:
        try:
            capture = json.loads(Path(args.verify).read_text())
            report, failures = evaluate(capture)
        except (OSError, ValueError, KeyError, TypeError, AttributeError) as error:
            print(f"compat_surfaces_check: {args.verify}: unreadable capture: {error!r}", file=sys.stderr)
            return 1
    elif args.run:
        capture = fetch(args.run, args.attempt)
        if args.captures:
            Path(args.captures).mkdir(parents=True, exist_ok=True)
            if not leaks(capture):
                Path(args.captures, "surfaces-capture.json").write_text(json.dumps(capture, indent=1) + "\n")
        report, failures = evaluate(capture)
        if args.out and not leaks(capture):
            Path(args.out).write_text(json.dumps(report, indent=1) + "\n")
    else:
        parser.error("give --run or --verify")
    for failure in failures:
        print(f"FAIL: {failure}", file=sys.stderr)
    for scope, observed in report["scopes"].items():
        print(f"{scope}: {'ok' if not observed['problems'] else '; '.join(observed['problems'])}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
