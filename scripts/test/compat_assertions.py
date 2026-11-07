#!/usr/bin/env python3
"""Named assertions of a #251 paired report, recomputed from its stored data.

A matrix row cites one as `report:<path>#<id>`. An assertion passes only when
every job it names is present on both lanes of every platform that runs it,
equal in conclusion and ordered steps, meets its expectation, and is not merely
acknowledged; plus its own predicates over observations, host data and leak
counts. `toolu_only` assertions measure toolu alone and are never reference
evidence. Nothing here trusts a stored `passed` flag.
"""

import json

# id -> rows it backs, jobs (scoped per variant), and extra predicates by name.
ASSERTIONS = {
    "X-1": {"rows": ["67/X-1"], "jobs": {"probe": ["contexts", "consumer"],
                                          "negative": ["contexts", "consumer"]},
            "checks": ["results"]},
    "X-2": {"rows": ["67/X-2"], "jobs": {"probe": ["composite", "consumer"]}, "checks": ["results"]},
    "X-3": {"rows": ["67/X-3"], "jobs": {"probe": ["stack", "container", "cancel-container", "consumer"],
                                          "negative": ["stack", "container"]},
            "checks": ["results", "containers"], "os": "Linux"},
    "X-4": {"rows": ["67/X-4"], "jobs": {"probe": ["failure", "orphans", "timeout", "cancel"]},
            "checks": []},
    "X-5": {"rows": ["67/X-5"], "jobs": {"probe": ["leaks", "secrets"], "negative": ["secrets"]},
            "checks": ["leaks"]},
    "X-6": {"rows": ["67/X-6"], "jobs": {"probe": ["contexts", "consumer", "pool-a", "pool-b"]},
            "checks": []},
    "X-6-toolu": {"rows": [], "toolu_only": True, "jobs": {}, "checks": ["lifecycle"]},
    "R-cache": {"rows": ["103/R-cache"], "jobs": {"probe": ["services", "stack", "consumer"],
                                                   "negative": ["services"]},
                "checks": ["results"]},
    "R-artifacts": {"rows": ["103/R-artifacts"],
                    "jobs": {"probe": ["services", "container", "stack", "consumer"],
                             "negative": ["services", "container"]},
                    "checks": ["results", "payloads"]},
    "R-oidc": {"rows": ["103/R-oidc"], "jobs": {"probe": ["services"], "negative": ["services"]},
               "checks": []},
    "R-command-files": {"rows": ["103/R-command-files"],
                        "jobs": {"probe": ["contexts", "docker", "consumer"],
                                 "negative": ["contexts", "docker", "consumer"]},
                        "checks": ["results"]},
    "R-masking": {"rows": ["103/R-masking"], "jobs": {"probe": ["leaks", "secrets"]}, "checks": ["leaks"]},
    "R-acquire": {"rows": ["103/R-acquire"], "jobs": "all", "only": "missing job", "checks": ["runners"]},
    "R-complete": {"rows": ["103/R-complete"], "jobs": "all", "checks": ["results"]},
    "R-renew": {"rows": ["103/R-renew"], "jobs": {"probe": ["long"]}, "checks": ["renewals"]},
    "R-cancellation": {"rows": ["103/R-cancellation"], "jobs": {"probe": ["cancel", "cancel-container"]},
                       "checks": ["polls"]},
    "R-temp-cleanup": {"rows": ["103/R-temp-cleanup"], "jobs": {"probe": ["contexts", "consumer"],
                                                                 "negative": ["contexts", "consumer"]},
                       "checks": []},
}
LANES = ("toolu", "official")
VARIANTS = ("probe", "negative")
LIFECYCLE = ("fresh-1", "fresh-2", "ephemeral")


def scopes(spec, platforms, expected_jobs, only_os=None):
    """(scope, job) pairs an assertion covers on the platforms that run each job."""
    out = []
    for os_name, arch in platforms:
        if only_os and os_name != only_os:
            continue
        jobs = spec if spec != "all" else {v: list(expected_jobs[os_name]) for v in ("probe", "negative")}
        for variant, names in jobs.items():
            out += [(f"{variant}-{os_name}-{arch}", job) for job in names if job in expected_jobs[os_name]]
    return out


def job_problems(pairs, problems, only=None):
    """Problems touching these jobs, acknowledged or not: an acknowledged
    difference still keeps the assertion (and its rows) unverified. `only`
    keeps the messages starting with it (R-acquire judges presence alone)."""
    wanted = set(pairs)
    return [message for key, job, message in problems
            if (key, job) in wanted and (only is None or message.startswith(only))]


def lanes_of(report, platform):
    return ((report.get("host") or {}).get(platform) or {}).get("lanes") or {}


def same(left, right):
    """JSON equality, as reference_compare.same: `true` is not `1`, `1` is not `1.0`."""
    return json.dumps(left, sort_keys=True) == json.dumps(right, sort_keys=True)


def check_results(report, platforms):
    """Every required scope present, both lanes non-empty objects, JSON-equal."""
    results = report.get("results")
    if not isinstance(results, dict):
        return ["results is not an object"]
    found = []
    for key in [f"{variant}-{platform}" for platform in platforms for variant in VARIANTS]:
        scope = results.get(key)
        lanes = [scope.get(lane) if isinstance(scope, dict) else None for lane in LANES]
        if not all(isinstance(lane, dict) and lane for lane in lanes):
            found.append(f"observations missing or malformed in {key}")
        elif not same(*lanes):
            found.append(f"observations differ in {key}")
    return found


def check_containers(report, _platforms):
    return [f"leftover {c}" for c in report.get("leftover_containers") or []]


def check_payloads(report, _platforms):
    return list(report.get("payload_errors") or [])


def count(value):
    """A non-negative integer count, or None: `False`, `1.0`, `"0"` and absent
    are not evidence of anything."""
    return value if type(value) is int and value >= 0 else None


def eligible_jobs(report, platform, lane):
    """Paired jobs of `lane` on `platform` that ran, so have a log to scan."""
    os_name, arch = platform.split("-")
    return sum(1 for r in report.get("jobs") or []
               if (r.get("os"), r.get("arch")) == (os_name, arch) and r[lane]["conclusion"] != "skipped")


def check_leaks(report, platforms):
    """Both lanes: no disposable value in any job log, annotation or VM log, and
    every job that ran was scanned. Counts only; no value is ever echoed."""
    found = []
    for platform in platforms:
        for lane in LANES:
            scan = ((report.get("leaks") or {}).get(platform) or {}).get(lane)
            if not isinstance(scan, dict):
                found.append(f"{platform}: no {lane} leak scan")
                continue
            for kind in ("logs", "annotations", "unreadable"):
                if count(scan.get(kind)) != 0:
                    found.append(f"{platform}: {lane} {kind} count is not 0")
            if count(scan.get("jobs_scanned")) != eligible_jobs(report, platform, lane):
                found.append(f"{platform}: {lane} scanned {scan.get('jobs_scanned')!r} of "
                             f"{eligible_jobs(report, platform, lane)} jobs")
            if count(lanes_of(report, platform).get(lane, {}).get("disposable_hits")) != 0:
                found.append(f"{platform}: {lane} VM logs or _diag hit count is not 0")
    return found


def check_renewals(report, platforms):
    return [f"{platform}: {lane} renewals_max is not an integer of at least 2"
            for platform in platforms for lane in LANES
            if (count(lanes_of(report, platform).get(lane, {}).get("renewals_max")) or 0) < 2]


def check_polls(report, platforms):
    return [f"{platform}: toolu poll_failures is not 0" for platform in platforms
            if count(lanes_of(report, platform).get("toolu", {}).get("poll_failures")) != 0]


def check_runners(report, _platforms):
    return [f"{r['variant']}-{r.get('os')}-{r.get('arch')}/{r['job']}: no {lane} runner"
            for r in report.get("jobs") or [] for lane in LANES
            if r[lane]["conclusion"] != "skipped" and not r[lane].get("runner_name")]


def check_lifecycle(report, platforms):
    found = []
    for platform in platforms:
        jobs = (report.get("toolu_only") or {}).get(platform) or {}
        found += [f"{platform}: {job} {jobs.get(job)}" for job in LIFECYCLE if jobs.get(job) != "success"]
        ephemeral = (((report.get("host") or {}).get(platform) or {}).get("toolu_only") or {}).get("ephemeral")
        ephemeral = ephemeral if isinstance(ephemeral, dict) else {}
        # Identity, not equality: `False == 0` and `1 == True` in Python.
        if not (count(ephemeral.get("exit_code")) == 0 and ephemeral.get("state_removed") is True
                and ephemeral.get("runner_removed") is True):
            found.append(f"{platform}: ephemeral run did not exit 0, remove its state and leave GitHub")
    return found


CHECKS = {"results": check_results, "containers": check_containers, "payloads": check_payloads,
          "leaks": check_leaks, "renewals": check_renewals, "polls": check_polls,
          "runners": check_runners, "lifecycle": check_lifecycle}


def evaluate(report, problems, fatal, platforms, expected_jobs):
    """{id: {rows, toolu_only, jobs, passed, problems}} recomputed from `report`.
    `problems` are reference_compare.keyed_problems (scope, job, message);
    `fatal` (shape, unpaired) fails every assertion."""
    names = [f"{o}-{a}" for o, a in platforms]
    out = {}
    for ident, spec in ASSERTIONS.items():
        pairs = scopes(spec["jobs"], platforms, expected_jobs, spec.get("os"))
        found = list(fatal) + job_problems(pairs, problems, spec.get("only"))
        for name in spec["checks"]:
            found += CHECKS[name](report, names)
        out[ident] = {"rows": spec["rows"], "toolu_only": bool(spec.get("toolu_only")),
                      "jobs": [f"{k}/{j}" for k, j in pairs], "passed": not found, "problems": found}
    return out


def stored_disagreements(report, computed):
    """Assertions whose stored `passed` flag or rows differ from the recomputation."""
    stored = report.get("assertions") or {}
    return [f"assertion {i}: stored {stored.get(i, {}).get('passed')} != recomputed {c['passed']}"
            for i, c in computed.items()
            if stored.get(i, {}).get("passed") != c["passed"] or stored.get(i, {}).get("rows") != c["rows"]]
