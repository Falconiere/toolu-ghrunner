#!/usr/bin/env python3
"""reference_compare.verify against the recorded #103 paired run and mutations of it."""

import copy
import importlib
import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
reference_compare = importlib.import_module("reference_compare")

ROOT = Path(__file__).resolve().parents[2]
REPORT = json.loads((ROOT / "docs/compat-reference-103.json").read_text())


def job(report, variant, name):
    return next(r for r in report["jobs"] if r["variant"] == variant and r["job"] == name)


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
                         ("probe", "toolu", "contexts"))
        self.assertEqual(reference_compare.split_job_name("negative-official / docker"),
                         ("negative", "official", "docker"))
        self.assertIsNone(reference_compare.split_job_name("probe-other / docker"))


if __name__ == "__main__":
    unittest.main()
