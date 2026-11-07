#!/usr/bin/env python3
"""reference_compare.verify against the recorded #103 paired run and mutations of it."""

import copy
import importlib
import json
import os
import stat
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
reference_compare = importlib.import_module("reference_compare")
compat_assertions = importlib.import_module("compat_assertions")

ROOT = Path(__file__).resolve().parents[2]
REPORT = json.loads((ROOT / "docs/compat-reference-103.json").read_text())


def job(report, variant, name):
    return next(r for r in report["jobs"] if r["variant"] == variant and r["job"] == name)


def expected_view(variant, name, recorded):
    """A lane view meeting `name`'s expectation; recorded #103 views where they already do."""
    conclusion, failed = reference_compare.expected(variant, name)
    if name in reference_compare.LEGACY_JOBS or (conclusion == "success" and not failed):
        return recorded
    steps = [["Set up job", "success"]] + [[step, "failure"] for step in failed]
    if conclusion == "skipped":
        steps = []
    return {**recorded, "conclusion": conclusion or "failure", "steps": steps}


def paired_report():
    """The recorded official lane cloned onto both lanes of every platform: a clean report."""
    report = copy.deepcopy(REPORT)
    report.update(issue=230, acknowledged=[], jobs=[], results={}, host={}, leaks={}, toolu_only={})
    report["run"]["id"] = 1
    for os_name, arch in reference_compare.EXPECTED_PLATFORMS:
        for name in reference_compare.EXPECTED_JOBS[os_name]:
            # The recorded run predates the #237 jobs: they borrow the services row.
            for row in REPORT["jobs"]:
                if row["job"] == (name if name in reference_compare.LEGACY_JOBS else "services"):
                    official = expected_view(row["variant"], name, row["official"])
                    report["jobs"].append({**copy.deepcopy(row), "job": name, "os": os_name,
                                           "arch": arch, "toolu": copy.deepcopy(official),
                                           "official": copy.deepcopy(official),
                                           "equal": True, "diff": []})
        for variant in reference_compare.VARIANTS:
            observed = REPORT["results"][variant]["official"]
            report["results"][reference_compare.scope(variant, os_name, arch)] = {
                "equal": True, "toolu": copy.deepcopy(observed), "official": copy.deepcopy(observed)}
        platform = f"{os_name}-{arch}"
        report["host"][platform] = clean_host()
        report["leaks"][platform] = {lane: {"logs": 0, "annotations": 0, "unreadable": 0}
                                     for lane in reference_compare.LANES}
        report["toolu_only"][platform] = {job: "success" for job in ("fresh-1", "fresh-2", "ephemeral")}
    for platform, lanes in report["leaks"].items():
        for lane, scan in lanes.items():
            scan["jobs_scanned"] = compat_assertions.eligible_jobs(report, platform, lane)
    report["assertions"] = reference_compare.assertions(report)
    return report


def clean_host():
    """One VM's host-251.json with nothing wrong in it."""
    lane = {"disposable_hits": 0, "renewals_max": 2, "poll_failures": 0}
    return {"provenance": {}, "lanes": {"toolu": dict(lane), "official": dict(lane)},
            "toolu_only": {"ephemeral": {"exit_code": 0, "state_removed": True, "runner_removed": True}}}


def platform_row(report, variant, name, os_name, arch):
    return next(r for r in report["jobs"] if (r["variant"], r["job"], r["os"], r["arch"])
                == (variant, name, os_name, arch))


def api_job(name, conclusion="success", steps=(("Run", "success"),)):
    """One job in the shape of GET /repos/{repo}/actions/runs/{id}/jobs."""
    return {"name": name, "conclusion": conclusion, "html_url": f"https://example.invalid/{name}",
            "runner_name": "r", "steps": [{"name": n, "conclusion": c} for n, c in steps]}


class FetchFailureCategoryTest(unittest.TestCase):
    def test_only_a_bounded_label_survives_a_failed_read(self):
        stderr = ("gh: Forbidden (HTTP 403)\nhttps://results.example/logs?sig=SECRETSIG&se=1 "
                  "token ghs_EXAMPLEtokenEXAMPLEtoken")
        label = reference_compare.failure_category(stderr)
        self.assertEqual(label, "http-403")
        self.assertEqual(reference_compare.failure_category("connection reset by peer"), "error")
        for secret in ("SECRETSIG", "ghs_", "https://"):
            self.assertNotIn(secret, label)


class VerifyRecordedReportTest(unittest.TestCase):
    def test_recorded_report_has_no_unacknowledged_difference(self):
        problems, notes = reference_compare.verify(REPORT)
        self.assertEqual(problems, [])
        self.assertTrue(notes, "the recorded run carries acknowledged differences")

    def test_recorded_contexts_and_services_jobs_are_equal(self):
        for variant in ("probe", "negative"):
            for name in ("contexts", "services"):
                self.assertTrue(job(REPORT, variant, name)["equal"], f"{variant}/{name}")
        self.assertTrue(all(r["equal"] for r in REPORT["results"].values()))

    def test_unacknowledged_difference_fails(self):
        report = copy.deepcopy(REPORT)
        report["acknowledged"] = [a for a in report["acknowledged"]
                                  if a["finding"] != "docker-pull-step-reporting"]
        problems, _ = reference_compare.verify(report)
        self.assertTrue(any("probe/docker" in p for p in problems), problems)

    def test_stale_acknowledgement_fails(self):
        report = copy.deepcopy(REPORT)
        ack = copy.deepcopy(report["acknowledged"][0])
        ack.update(finding="fixed-long-ago", job="contexts")
        report["acknowledged"].append(ack)
        problems, _ = reference_compare.verify(report)
        self.assertIn("stale acknowledgement fixed-long-ago: no matching difference", problems)

    def test_acknowledgement_without_matrix_rows_fails(self):
        report = copy.deepcopy(REPORT)
        report["acknowledged"][0]["matrix_rows"] = []
        problems, _ = reference_compare.verify(report)
        self.assertTrue(any("lacks diagnosis/matrix_rows" in p for p in problems), problems)

    def test_newly_unequal_job_fails(self):
        report = copy.deepcopy(REPORT)
        job(report, "probe", "contexts")["toolu"]["conclusion"] = "failure"
        problems, _ = reference_compare.verify(report)
        self.assertTrue(any(p.startswith("probe/contexts") for p in problems), problems)

    def test_editing_stored_flags_cannot_hide_a_difference(self):
        report = copy.deepcopy(REPORT)
        report["acknowledged"] = []
        for row in report["jobs"]:
            row["equal"], row["diff"] = True, []
        report["expectation_failures"] = []
        for result in report["results"].values():
            result["equal"] = True
        problems, _ = reference_compare.verify(report)
        self.assertTrue(any(p.startswith("probe/consumer") for p in problems), problems)
        self.assertTrue(any(p.startswith("probe/docker") for p in problems), problems)

    def test_new_divergence_in_an_acknowledged_job_is_not_excused(self):
        report = copy.deepcopy(REPORT)
        steps = job(report, "probe", "consumer")["toolu"]["steps"]
        steps[-1] = [steps[-1][0], "failure"]
        problems, _ = reference_compare.verify(report)
        self.assertTrue(any(p.startswith("probe/consumer") for p in problems), problems)

    def test_missing_job_leftover_container_and_changed_capture_fail(self):
        report = copy.deepcopy(REPORT)
        report["jobs"] = [r for r in report["jobs"] if r["job"] != "services"]
        report["leftover_containers"] = ["abc123 alpine@sha256:d9e8"]
        path = next(iter(report["captures"]))
        report["captures"][path] = "0" * 64
        problems, _ = reference_compare.verify(report)
        self.assertIn("probe/services: missing job", problems)
        self.assertTrue(any("leftover container" in p for p in problems), problems)
        self.assertIn(f"{path} changed since the run; re-record", problems)

    def test_a_failing_tool_exits_with_its_name(self):
        with self.assertRaises(SystemExit) as missing:
            reference_compare.run_tool(["no-such-tool-103", "x"])
        self.assertIn("no-such-tool-103 is not on PATH", str(missing.exception.code))
        with self.assertRaises(SystemExit) as failed:
            reference_compare.run_tool(["python3", "-c", "import sys; sys.exit('boom')"])
        self.assertIn("boom", str(failed.exception.code))

    def test_job_names_normalize_only_the_lane_token(self):
        self.assertEqual(reference_compare.split_job_name("probe-toolu / contexts (alpha, 7, true)"),
                         ("probe", "toolu", "contexts", None, None))
        self.assertEqual(reference_compare.split_job_name("negative-official / docker"),
                         ("negative", "official", "docker", None, None))
        self.assertIsNone(reference_compare.split_job_name("probe-other / docker"))

    def test_job_names_carry_the_platform(self):
        self.assertEqual(
            reference_compare.split_job_name("probe-Linux-ARM64-toolu / contexts (alpha, 7, true)"),
            ("probe", "toolu", "contexts", "Linux", "ARM64"))
        self.assertEqual(reference_compare.split_job_name("negative-macOS-ARM64-official / consumer"),
                         ("negative", "official", "consumer", "macOS", "ARM64"))


class PlatformPairingTest(unittest.TestCase):
    def test_clean_three_platform_report_verifies(self):
        self.assertEqual(reference_compare.verify(paired_report()), ([], []))

    def test_architectures_do_not_collapse_when_pairing(self):
        rows, unpaired = reference_compare.compare_jobs([
            api_job("probe-Linux-X64-toolu / contexts (alpha, 7, true)"),
            api_job("probe-Linux-X64-official / contexts (alpha, 7, true)"),
            api_job("probe-Linux-ARM64-toolu / contexts (alpha, 7, true)", "failure"),
            api_job("probe-Linux-ARM64-official / contexts (alpha, 7, true)"),
        ])
        self.assertEqual(unpaired, [])
        self.assertEqual({(r["os"], r["arch"]): r["equal"] for r in rows},
                         {("Linux", "X64"): True, ("Linux", "ARM64"): False})

    def test_mismatch_on_one_architecture_only_fails_there(self):
        report = paired_report()
        platform_row(report, "probe", "contexts", "Linux", "ARM64")["toolu"]["conclusion"] = "failure"
        problems, _ = reference_compare.verify(report)
        self.assertTrue(any(p.startswith("probe-Linux-ARM64/contexts") for p in problems), problems)
        self.assertFalse(any(p.startswith("probe-Linux-X64/") for p in problems), problems)

    def test_duplicate_job_stops_the_live_comparison(self):
        name = "probe-Linux-X64-toolu / contexts (alpha, 7, true)"
        with self.assertRaises(SystemExit) as stopped:
            reference_compare.compare_jobs([api_job(name), api_job(name)])
        self.assertIn("duplicate job", str(stopped.exception.code))

    def test_duplicate_row_in_a_report_fails(self):
        report = paired_report()
        report["jobs"].append(copy.deepcopy(report["jobs"][0]))
        problems, _ = reference_compare.verify(report)
        self.assertTrue(any("duplicate job" in p for p in problems), problems)

    def test_stripped_platform_fields_fail_outside_the_recorded_run(self):
        report = paired_report()
        for row in report["jobs"]:
            del row["os"], row["arch"]
        problems, _ = reference_compare.verify(report)
        self.assertTrue(any("unexpected platform" in p for p in problems), problems)

    def test_only_the_recorded_run_keeps_the_legacy_shape(self):
        report = copy.deepcopy(REPORT)
        report["run"]["id"] = 1
        problems, _ = reference_compare.verify(report)
        self.assertTrue(any("unexpected platform" in p for p in problems), problems)

    def test_missing_platform_fails(self):
        report = paired_report()
        report["jobs"] = [r for r in report["jobs"] if r["arch"] != "ARM64"]
        problems, _ = reference_compare.verify(report)
        self.assertIn("probe-Linux-ARM64/contexts: missing job", problems)
        self.assertIn("probe-macOS-ARM64/contexts: missing job", problems)

    def test_acknowledgement_on_one_platform_does_not_excuse_another(self):
        report = paired_report()
        for arch in ("X64", "ARM64"):
            row = platform_row(report, "probe", "docker", "Linux", arch)
            row["toolu"]["steps"] = row["toolu"]["steps"][:-1]
        report["acknowledged"] = reference_compare.pin_observations(report, [{
            "finding": "x64-only", "job": "docker", "variants": ["probe-Linux-X64"],
            "diagnosis": "diagnosed", "matrix_rows": ["103/S3"]}])
        problems, notes = reference_compare.verify(report)
        self.assertTrue(any(n.startswith("acknowledged [x64-only] probe-Linux-X64/docker")
                            for n in notes), notes)
        self.assertTrue(any(p.startswith("probe-Linux-ARM64/docker") for p in problems), problems)

    def test_acknowledgement_without_a_pin_cannot_excuse_a_missing_job(self):
        report = copy.deepcopy(REPORT)
        report["jobs"] = [r for r in report["jobs"]
                          if not (r["variant"] == "probe" and r["job"] == "consumer")]
        for ack in report["acknowledged"]:
            ack.pop("observed", None)
        problems, _ = reference_compare.verify(report)
        self.assertIn("probe/consumer: missing job", problems)


class PerPlatformJobsTest(unittest.TestCase):
    def test_macos_needs_no_docker_job_but_linux_does(self):
        report = paired_report()
        self.assertFalse([r for r in report["jobs"] if r["os"] == "macOS" and r["job"] == "docker"])
        self.assertEqual(reference_compare.verify(report)[0], [])
        report["jobs"].remove(platform_row(report, "probe", "docker", "Linux", "X64"))
        self.assertIn("probe-Linux-X64/docker: missing job", reference_compare.verify(report)[0])

    def test_surface_probe_jobs_are_required_per_platform(self):
        report = paired_report()
        report["jobs"].remove(platform_row(report, "negative", "container", "Linux", "ARM64"))
        report["jobs"].remove(platform_row(report, "probe", "annotations", "macOS", "ARM64"))
        problems = reference_compare.verify(report)[0]
        self.assertIn("negative-Linux-ARM64/container: missing job", problems)
        self.assertIn("probe-macOS-ARM64/annotations: missing job", problems)

    def test_container_job_that_ran_on_macos_fails(self):
        report = paired_report()
        ran = copy.deepcopy(platform_row(report, "probe", "container", "Linux", "X64"))
        ran.update(os="macOS", arch="ARM64")
        report["jobs"].append(ran)
        problems, _ = reference_compare.verify(report)
        self.assertTrue(any("unexpected job container" in p for p in problems), problems)

    def test_docker_job_that_ran_on_macos_fails(self):
        report = paired_report()
        ran = copy.deepcopy(platform_row(report, "probe", "docker", "Linux", "X64"))
        ran.update(os="macOS", arch="ARM64")
        report["jobs"].append(ran)
        problems, _ = reference_compare.verify(report)
        self.assertTrue(any("unexpected job docker" in p for p in problems), problems)

    def test_skipped_macos_docker_is_dropped_only_when_both_lanes_skipped(self):
        toolu, official = ("probe-macOS-ARM64-toolu / docker", "probe-macOS-ARM64-official / docker")
        rows, _ = reference_compare.compare_jobs([api_job(toolu, "skipped"), api_job(official, "skipped")])
        self.assertEqual(rows, [])
        rows, _ = reference_compare.compare_jobs([api_job(toolu), api_job(official, "skipped")])
        self.assertEqual(len(rows), 1)

    def test_skipped_linux_docker_is_not_dropped(self):
        rows, _ = reference_compare.compare_jobs([api_job("probe-Linux-X64-toolu / docker", "skipped"),
                                                  api_job("probe-Linux-X64-official / docker", "skipped")])
        self.assertEqual(len(rows), 1)


class StrictReportTest(unittest.TestCase):
    def problems_with(self, **fields):
        report = paired_report()
        report.update(fields)
        return reference_compare.verify(report)[0]

    def test_empty_or_missing_run_and_provenance_fail(self):
        for field in ("run", "provenance"):
            self.assertTrue(any(field in p for p in self.problems_with(**{field: {}})), field)
            report = paired_report()
            del report[field]
            self.assertTrue(any(field in p for p in reference_compare.verify(report)[0]), field)

    def test_run_missing_a_field_fails(self):
        report = paired_report()
        report["run"]["head_sha"] = ""
        self.assertTrue(any("head_sha" in p for p in reference_compare.verify(report)[0]))

    def test_empty_or_missing_results_fail(self):
        self.assertIn("probe-Linux-X64/*: toolu observations missing or empty",
                      self.problems_with(results={}))
        report = paired_report()
        del report["results"]
        self.assertIn("negative-macOS-ARM64/*: official observations missing or empty",
                      reference_compare.verify(report)[0])

    def test_both_lanes_empty_is_not_equal_evidence(self):
        report = paired_report()
        report["results"]["probe-Linux-X64"].update(toolu={}, official={})
        self.assertIn("probe-Linux-X64/*: toolu observations missing or empty",
                      reference_compare.verify(report)[0])

    def test_absent_list_fields_fail_without_crashing(self):
        for field in ("jobs", "unpaired", "payload_errors", "leftover_containers"):
            report = paired_report()
            del report[field]
            problems = reference_compare.verify(report)[0]
            self.assertTrue(any(field in p for p in problems), field)

    def test_recorded_report_with_erased_sections_fails(self):
        for field, needle in (("results", "observations missing or empty"), ("run", "run lacks"),
                              ("provenance", "provenance is missing or empty")):
            report = copy.deepcopy(REPORT)
            report[field] = {}
            problems, _ = reference_compare.verify(report)
            self.assertTrue(any(needle in p for p in problems), problems)

    def observations_differ(self, toolu, official):
        report = paired_report()
        report["results"]["probe-Linux-X64"].update(toolu=toolu, official=official)
        return "probe-Linux-X64/*: observations differ" in reference_compare.verify(report)[0]

    def test_json_types_are_compared(self):
        self.assertTrue(self.observations_differ({"v": True}, {"v": 1}))
        self.assertTrue(self.observations_differ({"v": "1"}, {"v": 1}))
        self.assertTrue(self.observations_differ({"v": 1}, {"v": 1.0}))
        self.assertTrue(self.observations_differ({"v": None, "w": 1}, {"w": 1}))

    def test_key_order_is_not_a_difference(self):
        self.assertFalse(self.observations_differ({"a": 1, "b": 2}, {"b": 2, "a": 1}))


class LeftoverContainersTest(unittest.TestCase):
    def test_only_probe_image_containers_count(self):
        listing = ("abc alpine@sha256:d9e853e87e55526f6b2917df91a2115c36dd7c696a35be12163d44e6e2a4b6bc\n"
                   "def alpine:3.20\n"
                   "ghi nginx:latest\n")
        self.assertEqual(reference_compare.probe_containers(listing, "Linux-X64"), [
            "Linux-X64: abc alpine@sha256:d9e853e87e55526f6b2917df91a2115c36dd7c696a35be12163d44e6e2a4b6bc",
            "Linux-X64: def alpine:3.20"])

    def test_missing_host_artifact_stops_the_comparison(self):
        with tempfile.TemporaryDirectory() as tools, tempfile.TemporaryDirectory() as tmp:
            gh = Path(tools, "gh")
            gh.write_text("#!/bin/sh\necho 'no valid artifacts found to download' >&2\nexit 1\n")
            gh.chmod(gh.stat().st_mode | stat.S_IEXEC)
            with mock.patch.dict(os.environ, {"PATH": tools}):
                with self.assertRaises(SystemExit) as stopped:
                    reference_compare.leftover_containers(1, tmp)
        self.assertIn("no valid artifacts", str(stopped.exception.code))


class CommandLineTest(unittest.TestCase):
    def test_live_mode_requires_an_output_path(self):
        with mock.patch("sys.stderr"), self.assertRaises(SystemExit) as stopped:
            reference_compare.main(["123"])
        self.assertEqual(stopped.exception.code, 2)

    def test_live_mode_refuses_to_overwrite_a_report(self):
        report = ROOT / "docs/compat-reference-103.json"
        before = report.read_bytes()
        with mock.patch("sys.stderr"), self.assertRaises(SystemExit) as stopped:
            reference_compare.main(["123", "--out", str(report)])
        self.assertEqual(stopped.exception.code, 2)
        self.assertEqual(report.read_bytes(), before)


if __name__ == "__main__":
    unittest.main()
