"""compat_assertions over the clean three-platform report and single mutations of it (#251)."""

import copy
import re
import importlib
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
reference_compare = importlib.import_module("reference_compare")
compat_assertions = importlib.import_module("compat_assertions")
fixtures = importlib.import_module("test_reference_compare")


def failing(report):
    """Assertion ids that do not pass, recomputed."""
    return sorted(i for i, a in reference_compare.assertions(report).items() if not a["passed"])


def row(report, variant, job, platform="Linux-X64"):
    os_name, arch = platform.split("-")
    return fixtures.platform_row(report, variant, job, os_name, arch)


class AssertionTest(unittest.TestCase):
    def setUp(self):
        self.report = fixtures.paired_report()

    def test_clean_report_passes_every_assertion(self):
        self.assertEqual(failing(self.report), [])
        self.assertEqual(reference_compare.verify(self.report), ([], []))

    def test_every_required_row_is_backed_by_one_assertion(self):
        rows = {r for a in compat_assertions.ASSERTIONS.values() for r in a["rows"]}
        wanted = {f"67/X-{i}" for i in range(1, 7)} | {
            f"103/R-{n}" for n in ("cache", "artifacts", "oidc", "command-files", "masking", "acquire",
                                   "renew", "complete", "cancellation", "temp-cleanup")}
        self.assertEqual(rows, wanted)

    def test_toolu_only_assertion_backs_no_row(self):
        lifecycle = compat_assertions.ASSERTIONS["X-6-toolu"]
        self.assertTrue(lifecycle["toolu_only"])
        self.assertEqual(lifecycle["rows"], [])

    def test_a_disposable_value_in_a_toolu_log_fails_masking_only(self):
        self.report["leaks"]["macOS-ARM64"]["toolu"]["logs"] = 1
        self.assertEqual(failing(self.report), ["R-masking", "X-5"])

    def test_a_leak_on_the_vm_fails_masking(self):
        self.report["host"]["Linux-ARM64"]["lanes"]["toolu"]["disposable_hits"] = 2
        self.assertEqual(failing(self.report), ["R-masking", "X-5"])

    def test_unscanned_logs_are_not_evidence_of_no_leak(self):
        self.report["leaks"]["Linux-X64"]["toolu"]["jobs_scanned"] = 0
        self.assertEqual(failing(self.report), ["R-masking", "X-5"])

    def assert_leak_evidence_rejected(self, mutate):
        mutate(self.report)
        self.assertEqual(failing(self.report), ["R-masking", "X-5"])
        problems = reference_compare.assertions(self.report)["X-5"]["problems"]
        self.assertTrue(problems)
        # Counts only: no 32-hex disposable value (or any long hex) in a message.
        self.assertFalse([p for p in problems if re.search(r"[0-9a-f]{32}", p)])

    def test_an_official_leak_in_a_log_fails(self):
        self.assert_leak_evidence_rejected(lambda r: r["leaks"]["Linux-X64"]["official"].update(logs=1))

    def test_an_official_leak_on_the_vm_fails(self):
        self.assert_leak_evidence_rejected(
            lambda r: r["host"]["Linux-X64"]["lanes"]["official"].update(disposable_hits=1))

    def test_a_missing_lane_scan_fails(self):
        self.assert_leak_evidence_rejected(lambda r: r["leaks"]["Linux-X64"].pop("official"))

    def test_a_missing_vm_hit_count_fails(self):
        self.assert_leak_evidence_rejected(
            lambda r: r["host"]["macOS-ARM64"]["lanes"]["toolu"].pop("disposable_hits"))

    def test_a_boolean_count_is_not_zero(self):
        self.assert_leak_evidence_rejected(lambda r: r["leaks"]["Linux-X64"]["toolu"].update(logs=False))

    def test_a_partial_scan_is_not_coverage(self):
        self.assert_leak_evidence_rejected(lambda r: r["leaks"]["Linux-X64"]["toolu"].update(jobs_scanned=1))

    def test_an_unreadable_log_is_not_coverage(self):
        self.assert_leak_evidence_rejected(lambda r: r["leaks"]["Linux-ARM64"]["official"].update(unreadable=1))

    def test_a_boolean_renewal_or_poll_count_fails(self):
        self.report["host"]["Linux-X64"]["lanes"]["toolu"]["renewals_max"] = True
        self.report["host"]["Linux-ARM64"]["lanes"]["toolu"]["poll_failures"] = False
        self.assertEqual(failing(self.report), ["R-cancellation", "R-renew"])

    def test_a_missing_renewal_or_poll_count_fails(self):
        del self.report["host"]["macOS-ARM64"]["lanes"]["official"]["renewals_max"]
        del self.report["host"]["macOS-ARM64"]["lanes"]["toolu"]["poll_failures"]
        self.assertEqual(failing(self.report), ["R-cancellation", "R-renew"])

    def test_a_boolean_ephemeral_exit_code_fails(self):
        self.report["host"]["Linux-X64"]["toolu_only"]["ephemeral"]["exit_code"] = False
        self.assertEqual(failing(self.report), ["X-6-toolu"])
        self.report = fixtures.paired_report()
        self.report["host"]["Linux-X64"]["toolu_only"]["ephemeral"]["runner_removed"] = 1
        self.assertEqual(failing(self.report), ["X-6-toolu"])

    def test_a_json_type_difference_in_observations_fails(self):
        scope = self.report["results"]["probe-Linux-X64"]
        scope["toolu"] = {**scope["official"], "typed": True}
        scope["official"] = {**scope["official"], "typed": 1}
        self.assertIn("X-1", failing(self.report))
        self.assertIn("R-command-files", failing(self.report))

    def all_fail(self, mutate):
        mutate(self.report)
        self.assertEqual(failing(self.report), sorted(compat_assertions.ASSERTIONS))
        problems, _ = reference_compare.verify(self.report)
        self.assertTrue(problems)

    def test_empty_results_fail_every_assertion(self):
        self.all_fail(lambda r: r.update(results={}))

    def test_a_missing_result_scope_fails_every_assertion(self):
        self.all_fail(lambda r: r["results"].pop("probe-Linux-X64"))

    def test_a_result_scope_without_its_official_lane_fails_every_assertion(self):
        self.all_fail(lambda r: r["results"]["negative-macOS-ARM64"].pop("official"))

    def test_a_malformed_section_fails_every_assertion(self):
        for field in ("results", "host", "leaks", "toolu_only"):
            with self.subTest(field=field):
                self.report = fixtures.paired_report()
                self.all_fail(lambda r, f=field: r.update({f: []}))

    def test_a_current_report_cannot_borrow_the_legacy_identity(self):
        self.report["run"]["id"] = reference_compare.LEGACY_RUN_ID
        self.assertFalse(reference_compare.is_legacy(self.report))
        self.assertEqual(failing(self.report), [])
        self.report["issue"] = 103
        self.assertTrue(reference_compare.is_legacy(self.report))
        problems, _ = reference_compare.verify(self.report)
        self.assertTrue(any("unexpected platform" in p for p in problems), problems)

    def test_one_renewal_fails_renew_only(self):
        self.report["host"]["Linux-X64"]["lanes"]["official"]["renewals_max"] = 1
        self.assertEqual(failing(self.report), ["R-renew"])

    def test_mid_job_poll_failures_fail_cancellation(self):
        self.report["host"]["macOS-ARM64"]["lanes"]["toolu"]["poll_failures"] = 39
        self.assertEqual(failing(self.report), ["R-cancellation"])

    def test_a_cancel_that_ends_differently_fails_cancel_rows(self):
        view = row(self.report, "probe", "cancel")["toolu"]
        view["conclusion"] = "failure"
        self.assertEqual(failing(self.report), ["R-cancellation", "R-complete", "X-4"])

    def test_failed_lifecycle_fails_only_the_toolu_only_assertion(self):
        self.report["toolu_only"]["Linux-ARM64"]["fresh-2"] = "failure"
        self.report["host"]["macOS-ARM64"]["toolu_only"]["ephemeral"]["runner_removed"] = False
        self.assertEqual(failing(self.report), ["X-6-toolu"])

    def test_an_acknowledged_difference_keeps_its_assertion_failing(self):
        stack = row(self.report, "probe", "stack")
        stack["toolu"]["steps"] = [["Set up job", "success"]]
        ack = {"finding": "probe", "job": "stack", "variants": ["probe-Linux-X64"],
               "diagnosis": "d", "matrix_rows": ["67/X-3"]}
        self.report["acknowledged"] = reference_compare.pin_observations(self.report, [ack])
        self.report["assertions"] = reference_compare.assertions(self.report)
        problems, notes = reference_compare.verify(self.report)
        self.assertEqual(problems, [])
        self.assertTrue(notes)
        self.assertIn("X-3", failing(self.report))

    def test_a_stored_flag_that_disagrees_fails_verify(self):
        self.report["host"]["Linux-X64"]["lanes"]["toolu"]["renewals_max"] = 0
        problems, _ = reference_compare.verify(self.report)
        self.assertIn("assertion R-renew: stored True != recomputed False", problems)

    def test_a_missing_section_fails_verify(self):
        del self.report["host"]
        problems, _ = reference_compare.verify(self.report)
        self.assertIn("host is missing", problems)

    def test_assertions_name_platform_jobs_only_where_they_run(self):
        x3 = reference_compare.assertions(copy.deepcopy(self.report))["X-3"]["jobs"]
        self.assertIn("probe-Linux-ARM64/stack", x3)
        self.assertFalse([j for j in x3 if j.startswith(("probe-macOS", "negative-macOS"))])


if __name__ == "__main__":
    unittest.main()
