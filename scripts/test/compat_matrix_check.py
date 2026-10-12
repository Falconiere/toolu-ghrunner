#!/usr/bin/env python3
"""Validate the epic #67 compatibility acceptance matrix (#103).

Usage:
  compat_matrix_check.py [--check]          validate + verify the Markdown view (CI)
  compat_matrix_check.py --write            regenerate docs/compatibility-matrix.md
  compat_matrix_check.py --check-rows FILE  validate a JSON list of rows on its own
  compat_matrix_check.py --extract-ids N..  print deterministic AC/scenario ids (gh)
  compat_matrix_check.py --remote           compare ids/states/run links with GitHub

`docs/compatibility-matrix.json` is the source of truth. Every row maps one
acceptance criterion or scenario to its input, expected observable, production
path, tests/command and a status for EVERY lane. A lane is `passed` only with
evidence; any other status needs a reason. See docs/compatibility-evidence.md.
"""

import argparse
import importlib
import json
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
reference_compare = importlib.import_module("reference_compare")

ROOT = Path(__file__).resolve().parents[2]
MATRIX = ROOT / "docs/compatibility-matrix.json"
MARKDOWN = ROOT / "docs/compatibility-matrix.md"
EVIDENCE_DOC = ROOT / "docs/compatibility-evidence.md"
CI = ROOT / ".github/workflows/ci.yml"
REPO = "Falconiere/toolu-ghrunner"
EPIC = 67
CHILDREN = list(range(68, 104))
LANES = ("hermetic", "macos", "docker", "github-com", "reference", "ghes", "ui-service")
STATUSES = ("passed", "unverified", "not-applicable", "pending")
REQUIRED_REGRESSIONS = ("cache", "artifacts", "oidc", "command-files", "masking",
                        "acquire", "renew", "complete", "cancellation")
REQUIRED_DOC_SECTIONS = ("Lanes and statuses", "Capture and sanitization",
                         "Reference baseline provenance", "Recorded differences",
                         "Approved scope exclusions")
EPIC_ROWS = tuple(f"X-{i}" for i in range(1, 7))
RUN_URL = re.compile(rf"^https://github\.com/{REPO}/(actions/runs/\d+(/job/\d+)?|pull/\d+)$")
GATE_LANES = ("hermetic", "macos")


def heading_anchor(text):
    """GitHub's heading slug."""
    slug = re.sub(r"[^\w\- ]", "", text.strip().lower())
    return slug.replace(" ", "-")


def anchors(path):
    return {heading_anchor(line.lstrip("#")) for line in path.read_text().splitlines()
            if line.startswith("#")}


def test_attributes(source, name):
    """Attribute lines above `fn name(` if it is a test fn, else None."""
    lines = [line.strip() for line in source.splitlines()]
    for index, line in enumerate(lines):
        if not re.match(rf"(pub\s+)?(async\s+)?fn {re.escape(name)}\b", line):
            continue
        attrs = []
        for above in reversed(lines[:index]):
            if above.startswith("#[") or above.startswith("///") or above == "":
                attrs.append(above)
                continue
            break
        if any(a.startswith(("#[test]", "#[tokio::test")) for a in attrs):
            return attrs
    return None


def python_test(source, name):
    """True when `name` is a unittest method (`def test_…(self`) in `source`."""
    return name.startswith("test") and re.search(rf"^\s+def {re.escape(name)}\(self", source,
                                                  re.M) is not None


def check_tests(row, errors):
    """Validate row tests; returns one (kind, ignored, linux_only, file) tuple per valid test."""
    found = []
    for entry in row.get("tests", []):
        if not (isinstance(entry, list) and len(entry) == 2):
            errors.append(f"{row['id']}: test entry {entry!r} is not [path, fn]")
            continue
        path, name = entry
        file = ROOT / path
        if not file.is_file():
            errors.append(f"{row['id']}: test file {path} does not exist")
            continue
        source = file.read_text()
        if path.endswith(".py"):
            if python_test(source, name):
                found.append(("py", False, False, Path(path).name))
            else:
                errors.append(f"{row['id']}: {path}::{name} is not a unittest test method")
            continue
        attrs = test_attributes(source, name)
        if attrs is None:
            errors.append(f"{row['id']}: {path}::{name} is not a #[test] / #[tokio::test] fn")
            continue
        found.append(("rs", any(a.startswith("#[ignore") for a in attrs),
                      '#![cfg(target_os = "linux")]' in source, Path(path).stem))
    return found


def check_evidence(row, lane, evidence, tests, context, errors):
    where = f"{row['id']} [{lane}]"
    in_ci = [t for t in tests if t[0] == "py" and t[3] in context["ci"]]
    runnable = [t for t in tests if t[0] == "rs" and not t[1]] + in_ci
    if evidence == "gate":
        if lane not in GATE_LANES:
            errors.append(f"{where}: 'gate' evidence only backs hermetic/macos lanes")
        elif not tests:
            errors.append(f"{where}: 'gate' evidence needs at least one mapped test")
        elif not runnable:
            errors.append(f"{where}: an #[ignore] test never runs in the gate")
        elif lane == "macos" and not [t for t in runnable if t[0] == "rs" and not t[2]]:
            errors.append(f"{where}: the macOS gate needs a non-ignored Rust test that is not "
                          "Linux-only (Python checks run on the ubuntu leg only)")
    elif evidence == "ci-docker":
        docker_step = context["ci"].split("Docker action runtime", 1)[-1].split("- name:", 1)[0]
        if lane != "docker" or not [t for t in tests
                                    if t[0] == "rs" and f"--test {t[3]} " in docker_step]:
            errors.append(f"{where}: 'ci-docker' needs a mapped test run by ci.yml's Docker step")
    elif evidence.startswith(("run:", "pr:")):
        if not RUN_URL.match(evidence.split(":", 1)[1]):
            errors.append(f"{where}: evidence URL {evidence!r} is not a run/PR of {REPO}")
    elif evidence.startswith("doc:"):
        path, _, anchor = evidence[4:].partition("#")
        file = ROOT / path
        if not file.is_file() or (anchor and anchor not in anchors(file)):
            errors.append(f"{where}: {evidence!r} does not resolve to a document heading")
    elif evidence.startswith("report:"):
        report = context["reports"].get(evidence[7:])
        if report is None:
            errors.append(f"{where}: report {evidence[7:]} is missing")
        elif row["id"] in report["unverified_rows"]:
            errors.append(f"{where}: the report acknowledges a difference for this row")
    else:
        errors.append(f"{where}: unknown evidence kind {evidence!r}")


def check_row(row, issues, context, errors):
    rid = row.get("id", "?")
    for key in ("criterion", "expected", "production_path", "command"):
        if not isinstance(row.get(key), str) or not row[key].strip():
            errors.append(f"{rid}: missing {key}")
    for path in row.get("input", []):
        if not (ROOT / path.split("#")[0]).exists():
            errors.append(f"{rid}: input {path} does not exist")
    tests = check_tests(row, errors)
    lanes = row.get("lanes", {})
    if set(lanes) != set(LANES):
        errors.append(f"{rid}: lanes must be exactly {', '.join(LANES)}")
    # #103 delivers this matrix, so its own rows carry real statuses.
    open_issue = (issues.get(str(row.get("issue")), {}).get("state") == "open"
                  and str(row.get("issue")) != "103")
    for lane, value in lanes.items():
        status = value.get("status")
        if status not in STATUSES:
            errors.append(f"{rid} [{lane}]: status {status!r} is not one of {STATUSES}")
        elif status == "passed":
            if not value.get("evidence"):
                errors.append(f"{rid} [{lane}]: passed without evidence")
            else:
                check_evidence(row, lane, value["evidence"], tests, context, errors)
        elif not str(value.get("reason", "")).strip():
            errors.append(f"{rid} [{lane}]: {status} without a reason")
        if open_issue and (status != "pending" or
                           f"issues/{row['issue']}" not in str(value.get("reason", ""))):
            errors.append(f"{rid} [{lane}]: open issue rows must be pending with the issue link")


def check_coverage(matrix, errors):
    issues, rows = matrix.get("issues", {}), matrix.get("rows", [])
    if sorted(int(n) for n in issues) != CHILDREN:
        errors.append(f"issues must be exactly the epic children {CHILDREN[0]}..{CHILDREN[-1]}")
    seen = {}
    for row in rows:
        if row.get("id") in seen:
            errors.append(f"duplicate row id {row.get('id')}")
        seen[row.get("id")] = row
    for number, issue in issues.items():
        expected = {f"{number}/{i}" for i in issue.get("ids", [])}
        if not expected:
            errors.append(f"#{number}: no AC/scenario ids recorded")
        have = {rid for rid in seen if rid and rid.split("/")[0] == number}
        if number == "103":
            have = {rid for rid in have if "/R-" not in rid}
        for missing in sorted(expected - have):
            errors.append(f"#{number}: no row for {missing}")
        for extra in sorted(have - expected):
            errors.append(f"#{number}: row {extra} is not an id of the issue")
    for rid in [f"67/{x}" for x in EPIC_ROWS] + [f"103/R-{s}" for s in REQUIRED_REGRESSIONS]:
        if rid not in seen:
            errors.append(f"missing required row {rid}")
    return seen


def load_reports(matrix):
    reports = {}
    for path in matrix.get("reports", []):
        file = ROOT / path
        if not file.is_file():
            continue
        data = json.loads(file.read_text())
        rows = {r for ack in data.get("acknowledged", []) for r in ack.get("matrix_rows", [])}
        reports[path] = {"data": data, "unverified_rows": rows}
    return reports


SYMBOL = {"passed": "✅", "unverified": "⚠️", "not-applicable": "➖", "pending": "⏳"}


def cell(text):
    return " ".join(str(text).split()).replace("|", "\\|")


def lane_cell(value):
    status = value.get("status", "?")
    detail = value.get("evidence") if status == "passed" else value.get("reason", "")
    link = detail.split(":", 1)[1] if str(detail).startswith(("run:", "pr:")) else ""
    label = f"[{status}]({link})" if link else status
    return f"{SYMBOL.get(status, '')} {label}".strip()


def render(matrix):
    """The generated Markdown view of the matrix (never edit it by hand)."""
    rows = matrix.get("rows", [])
    counts = {lane: {s: 0 for s in STATUSES} for lane in LANES}
    for row in rows:
        for lane, value in row.get("lanes", {}).items():
            if lane in counts and value.get("status") in STATUSES:
                counts[lane][value["status"]] += 1
    out = ["# Compatibility acceptance matrix", "",
           "<!-- Generated by scripts/test/compat_matrix_check.py --write from "
           "docs/compatibility-matrix.json. Do not edit by hand. -->", "",
           f"Epic #{EPIC} children, one row per acceptance criterion or scenario. "
           "Status semantics, evidence kinds and the reference lane are described in "
           "[compatibility-evidence.md](compatibility-evidence.md). A lane that is not "
           "✅ passed is **not** coverage.", "",
           "| Lane | " + " | ".join(STATUSES) + " |",
           "| --- | " + " | ".join("---" for _ in STATUSES) + " |"]
    out += [f"| {lane} | " + " | ".join(str(counts[lane][s]) for s in STATUSES) + " |"
            for lane in LANES]
    groups = {}
    for row in rows:
        groups.setdefault(str(row.get("issue")), []).append(row)
    issues = matrix.get("issues", {})
    for number in sorted(groups, key=int):
        meta = issues.get(number, {"title": "Epic cross-feature workflows", "state": "open"})
        out += ["", f"## #{number} — {cell(meta.get('title', ''))}", "",
                f"State: {meta.get('state', '?')}"
                + (f" · PR: {meta['pr']}" if meta.get("pr") else ""), "",
                "| ID | Criterion | Expected | Tests / command | "
                + " | ".join(LANES) + " |",
                "| --- | --- | --- | --- | " + " | ".join("---" for _ in LANES) + " |"]
        for row in groups[number]:
            tests = "<br>".join(f"`{Path(p).name}::{n}`" for p, n in row.get("tests", []))
            tests = tests or f"`{cell(row.get('command', ''))}`"
            lanes = " | ".join(lane_cell(row.get("lanes", {}).get(lane, {})) for lane in LANES)
            out.append(f"| {row['id']} | {cell(row.get('criterion', ''))} | "
                       f"{cell(row.get('expected', ''))} | {tests} | {lanes} |")
    return "\n".join(out) + "\n"


def validate(matrix, markdown, evidence_doc):
    """Every violation in `matrix`, its Markdown view and the evidence doc."""
    errors = []
    context = {"ci": CI.read_text(), "reports": load_reports(matrix)}
    for path, report in context["reports"].items():
        problems, _ = reference_compare.verify(report["data"])
        errors += [f"report {path}: {p}" for p in problems]
    seen = check_coverage(matrix, errors)
    for path, report in context["reports"].items():
        for rid in sorted(report["unverified_rows"]):
            lane = seen.get(rid, {}).get("lanes", {}).get("reference", {})
            if lane.get("status") == "passed":
                errors.append(f"{rid}: {path} records an unfixed difference; reference cannot pass")
            if rid not in seen:
                errors.append(f"{path} names unknown matrix row {rid}")
    for row in matrix.get("rows", []):
        check_row(row, matrix.get("issues", {}), context, errors)
    doc_anchors = {heading_anchor(line.lstrip("#")) for line in evidence_doc.splitlines()
                   if line.startswith("#")}
    for section in REQUIRED_DOC_SECTIONS:
        if heading_anchor(section) not in doc_anchors:
            errors.append(f"docs/compatibility-evidence.md lacks the '{section}' section")
    if markdown != render(matrix):
        errors.append("docs/compatibility-matrix.md is stale; run compat_matrix_check.py --write")
    return errors


def extract_ids(number):
    """Deterministic ids from an issue body: AC-<n> per checkbox, table scenario ids."""
    issue = json.loads(subprocess.check_output(
        ["gh", "issue", "view", str(number), "--repo", REPO, "--json", "title,state,body"],
        text=True))
    ids, section, ac = [], "", 0
    for line in issue["body"].splitlines():
        if line.startswith("## "):
            section = line[3:].strip().lower()
        elif section == "acceptance criteria" and re.match(r"\s*- \[[ xX]\] ", line):
            ac += 1
            ids.append(f"AC-{ac}")
        else:
            cell = re.match(r"\|\s*((?:\d+-)?S\d+[a-z]?)\s*\|", line)
            if cell and cell.group(1) not in ids:
                ids.append(cell.group(1))
    return {"title": issue["title"], "state": issue["state"].lower(), "ids": ids}


def remote(matrix):
    errors = []
    for number, issue in matrix["issues"].items():
        live = extract_ids(number)
        if live["ids"] != issue["ids"]:
            errors.append(f"#{number}: ids changed on GitHub: {live['ids']}")
        if live["state"] != issue["state"]:
            errors.append(f"#{number}: state is {live['state']} on GitHub")
    runs = {v["evidence"].split("/actions/runs/")[1].split("/")[0]
            for row in matrix["rows"] for v in row["lanes"].values()
            if "/actions/runs/" in str(v.get("evidence", ""))}
    for run in sorted(runs):
        out = subprocess.run(["gh", "api", f"repos/{REPO}/actions/runs/{run}", "--jq", ".status"],
                             capture_output=True, text=True)
        if out.returncode != 0 or out.stdout.strip() != "completed":
            errors.append(f"run {run} is not a completed run of {REPO}")
    return errors


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true")
    mode.add_argument("--write", action="store_true")
    mode.add_argument("--check-rows", type=Path)
    mode.add_argument("--extract-ids", type=int, nargs="+")
    mode.add_argument("--remote", action="store_true")
    args = parser.parse_args(argv)
    if args.extract_ids:
        print(json.dumps({str(n): extract_ids(n) for n in args.extract_ids}, indent=2))
        return 0
    matrix = json.loads(MATRIX.read_text())
    if args.write:
        MARKDOWN.write_text(render(matrix))
        print(f"wrote {MARKDOWN.relative_to(ROOT)}")
        return 0
    if args.check_rows:
        errors = []
        context = {"ci": CI.read_text(), "reports": load_reports(matrix)}
        for row in json.loads(args.check_rows.read_text()):
            check_row(row, matrix.get("issues", {}), context, errors)
    elif args.remote:
        errors = remote(matrix)
    else:
        markdown = MARKDOWN.read_text() if MARKDOWN.is_file() else ""
        doc = EVIDENCE_DOC.read_text() if EVIDENCE_DOC.is_file() else ""
        errors = validate(matrix, markdown, doc)
    for error in errors:
        print(error, file=sys.stderr)
    print(f"{len(errors)} problem(s)" if errors else "compatibility matrix OK")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
