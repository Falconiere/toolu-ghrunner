#!/usr/bin/env python3
"""compat_matrix_check.validate against the committed matrix and mutations of it."""

import copy
import importlib
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
check = importlib.import_module("compat_matrix_check")
report_fixtures = importlib.import_module("test_reference_compare")

MATRIX = json.loads(check.MATRIX.read_text())
MARKDOWN = check.MARKDOWN.read_text()
EVIDENCE_DOC = check.EVIDENCE_DOC.read_text()


def row(matrix, rid):
    return next(r for r in matrix["rows"] if r["id"] == rid)


def gate_row(matrix):
    """A row whose hermetic lane is backed by the gate."""
    return next(r for r in matrix["rows"]
                if r["lanes"]["hermetic"].get("evidence") == "gate"
                and all(p.endswith(".rs") for p, _ in r["tests"]))


def errors_for(matrix, markdown=None, doc=EVIDENCE_DOC):
    """Validate a mutated matrix against its own regenerated view unless given one."""
    return check.validate(matrix, markdown if markdown is not None else check.render(matrix), doc)


class CommittedMatrixTest(unittest.TestCase):
    def test_committed_matrix_is_valid(self):
        self.assertEqual(check.validate(MATRIX, MARKDOWN, EVIDENCE_DOC), [])

    def test_every_child_issue_and_id_has_exactly_one_row(self):
        ids = [r["id"] for r in MATRIX["rows"]]
        self.assertEqual(len(ids), len(set(ids)))
        for number, issue in MATRIX["issues"].items():
            for ident in issue["ids"]:
                self.assertIn(f"{number}/{ident}", ids)

    def test_open_issues_other_than_103_are_pending(self):
        for r in MATRIX["rows"]:
            state = MATRIX["issues"].get(str(r["issue"]), {}).get("state")
            if state == "open" and r["issue"] != 103:
                self.assertEqual({v["status"] for v in r["lanes"].values()}, {"pending"}, r["id"])


class RejectionTest(unittest.TestCase):
    def assert_rejected(self, matrix, needle, **kwargs):
        errors = errors_for(matrix, **kwargs)
        self.assertTrue(any(needle in e for e in errors), f"{needle!r} not in {errors}")

    def test_missing_test_fn(self):
        m = copy.deepcopy(MATRIX)
        target = gate_row(m)
        target["tests"][0][1] = "no_such_test_fn_103"
        self.assert_rejected(m, "is not a #[test] / #[tokio::test] fn")

    def test_non_test_fn(self):
        m = copy.deepcopy(MATRIX)
        gate_row(m)["tests"] = [["crates/listener/src/capture.rs", "write_acquired"]]
        self.assert_rejected(m, "write_acquired is not a #[test]")

    def test_ignored_test_cannot_back_the_gate(self):
        m = copy.deepcopy(MATRIX)
        gate_row(m)["tests"] = [["crates/execution/tests/compat_probe_replay_test.rs",
                                 "captured_docker_job_executes_the_container_and_cleans_up"]]
        self.assert_rejected(m, "an #[ignore] or feature-gated test never runs in the gate")

    def test_feature_gated_test_cannot_back_the_gate(self):
        m = copy.deepcopy(MATRIX)
        gate_row(m)["tests"] = [["crates/toolu-runner/tests/live_e2e.rs", "noop_job_completes"]]
        self.assert_rejected(m, "feature-gated test never runs in the gate")

    def test_ci_docker_evidence_needs_a_test_file_the_docker_step_runs(self):
        m = copy.deepcopy(MATRIX)
        target = next(r for r in m["rows"]
                      if r["lanes"].get("docker", {}).get("evidence") == "ci-docker")
        target["tests"] = [["crates/execution/tests/shell_templates_container_test.rs",
                            "container_default_shell_is_sh_and_pipeline_succeeds"]]
        self.assert_rejected(m, "'ci-docker' needs a mapped test run by ci.yml's Docker step")

    def test_report_evidence_only_backs_the_reference_lane(self):
        m = copy.deepcopy(MATRIX)
        gate_row(m)["lanes"]["github-com"] = {"status": "passed",
                                              "evidence": "report:docs/compat-reference-103.json"}
        self.assert_rejected(m, "'report:' evidence only backs the reference lane")

    def test_invalid_listed_report(self):
        with tempfile.NamedTemporaryFile("w", suffix=".json", dir=check.ROOT / "docs",
                                         delete=False) as bad:
            bad.write("{not json")
        try:
            m = copy.deepcopy(MATRIX)
            m["reports"] = [str(Path(bad.name).relative_to(check.ROOT))]
            self.assert_rejected(m, "is not valid JSON")
        finally:
            Path(bad.name).unlink()

    def test_missing_listed_report(self):
        m = copy.deepcopy(MATRIX)
        m["reports"] = ["docs/no-such-report.json"]
        self.assert_rejected(m, "listed report docs/no-such-report.json does not exist")

    def test_passed_without_evidence(self):
        m = copy.deepcopy(MATRIX)
        gate_row(m)["lanes"]["hermetic"] = {"status": "passed"}
        self.assert_rejected(m, "passed without evidence")

    def test_non_passed_without_reason(self):
        m = copy.deepcopy(MATRIX)
        gate_row(m)["lanes"]["ghes"] = {"status": "unverified"}
        self.assert_rejected(m, "unverified without a reason")

    def test_foreign_evidence_url(self):
        m = copy.deepcopy(MATRIX)
        gate_row(m)["lanes"]["github-com"] = {"status": "passed",
                                              "evidence": "run:https://example.com/runs/1"}
        self.assert_rejected(m, "is not a run/PR of")

    def test_missing_child_issue(self):
        m = copy.deepcopy(MATRIX)
        del m["issues"]["86"]
        self.assert_rejected(m, "issues must be exactly the epic children")

    def test_missing_row_for_an_issue_id(self):
        m = copy.deepcopy(MATRIX)
        m["rows"] = [r for r in m["rows"] if r["id"] != "86/86-S1"]
        self.assert_rejected(m, "#86: no row for 86/86-S1")

    def test_duplicate_row_id(self):
        m = copy.deepcopy(MATRIX)
        m["rows"].append(copy.deepcopy(m["rows"][0]))
        self.assert_rejected(m, f"duplicate row id {m['rows'][0]['id']}")

    def test_missing_regression_surface(self):
        m = copy.deepcopy(MATRIX)
        m["rows"] = [r for r in m["rows"] if r["id"] != "103/R-cancellation"]
        self.assert_rejected(m, "missing required row 103/R-cancellation")

    def test_missing_lane(self):
        m = copy.deepcopy(MATRIX)
        del gate_row(m)["lanes"]["reference"]
        self.assert_rejected(m, "lanes must be exactly")

    def test_open_issue_row_must_stay_pending(self):
        m = copy.deepcopy(MATRIX)
        m["issues"]["94"]["state"] = "open"
        row(m, "94/AC-1")["lanes"]["hermetic"] = {"status": "passed", "evidence": "gate"}
        self.assert_rejected(m, "open issue rows must be pending with the issue link")

    def test_row_citing_the_report_that_acknowledges_it_cannot_pass(self):
        m = copy.deepcopy(MATRIX)
        row(m, "103/R-cache")["lanes"]["reference"] = {
            "status": "passed", "evidence": "report:docs/compat-reference-103.json"}
        self.assert_rejected(m, "103/R-cache [reference]: the report acknowledges a difference")

    def test_row_citing_a_fresh_report_passes_while_the_old_report_lists_it(self):
        with tempfile.NamedTemporaryFile("w", suffix=".json", dir=check.ROOT / "docs",
                                         delete=False) as fresh:
            json.dump(report_fixtures.paired_report(), fresh)
        try:
            path = str(Path(fresh.name).relative_to(check.ROOT))
            m = copy.deepcopy(MATRIX)
            m["reports"].append(path)
            self.assertIn("103/R-cache", {rid for ack in json.loads(
                (check.ROOT / "docs/compat-reference-103.json").read_text())["acknowledged"]
                for rid in ack["matrix_rows"]})
            row(m, "103/R-cache")["lanes"]["reference"] = {"status": "passed",
                                                           "evidence": f"report:{path}#R-cache"}
            self.assertEqual(errors_for(m), [])
        finally:
            Path(fresh.name).unlink()

    def cite(self, report, rid, evidence_suffix):
        """Errors for `rid`'s reference lane citing `report` (written under docs/)."""
        with tempfile.NamedTemporaryFile("w", suffix=".json", dir=check.ROOT / "docs",
                                         delete=False) as fresh:
            json.dump(report, fresh)
        try:
            path = str(Path(fresh.name).relative_to(check.ROOT))
            m = copy.deepcopy(MATRIX)
            m["reports"].append(path)
            row(m, rid)["lanes"]["reference"] = {"status": "passed",
                                                 "evidence": f"report:{path}{evidence_suffix}"}
            return errors_for(m)
        finally:
            Path(fresh.name).unlink()

    def test_a_251_report_must_be_cited_through_an_assertion(self):
        errors = self.cite(report_fixtures.paired_report(), "103/R-cache", "")
        self.assertTrue(any("cite one of the report's assertions" in e for e in errors), errors)

    def test_an_assertion_for_another_row_or_unknown_is_rejected(self):
        report = report_fixtures.paired_report()
        self.assertTrue(any("does not cover this row" in e
                            for e in self.cite(report, "103/R-cache", "#R-oidc")))
        self.assertTrue(any("has no assertion X-9" in e for e in self.cite(report, "67/X-1", "#X-9")))

    def test_a_toolu_only_assertion_never_backs_the_reference_lane(self):
        errors = self.cite(report_fixtures.paired_report(), "67/X-6", "#X-6-toolu")
        self.assertTrue(any("measures toolu alone" in e for e in errors), errors)

    def test_a_failing_assertion_is_rejected_even_if_stored_as_passed(self):
        report = report_fixtures.paired_report()
        report["host"]["Linux-X64"]["lanes"]["toolu"]["renewals_max"] = 0
        errors = self.cite(report, "103/R-renew", "#R-renew")
        self.assertTrue(any("assertion R-renew does not pass" in e for e in errors), errors)

    def test_row_citing_the_run_of_the_report_that_acknowledges_it_cannot_pass(self):
        m = copy.deepcopy(MATRIX)
        row(m, "103/R-cache")["lanes"]["reference"] = {
            "status": "passed",
            "evidence": "run:https://github.com/Falconiere/toolu-ghrunner/actions/runs/38301583727"}
        self.assert_rejected(m, "103/R-cache [reference]: the report of this run acknowledges")

    def test_longer_run_id_is_not_the_acknowledging_run(self):
        m = copy.deepcopy(MATRIX)
        row(m, "103/R-cache")["lanes"]["reference"] = {
            "status": "passed",
            "evidence": "run:https://github.com/Falconiere/toolu-ghrunner/actions/runs/383015837270"}
        self.assertEqual(errors_for(m), [])

    def test_reference_lane_takes_only_paired_runs_and_reports(self):
        for evidence in ("pr:https://github.com/Falconiere/toolu-ghrunner/pull/1",
                         "run:https://github.com/Falconiere/toolu-ghrunner/pull/1",
                         "doc:docs/compatibility-evidence.md"):
            m = copy.deepcopy(MATRIX)
            row(m, "103/R-cache")["lanes"]["reference"] = {"status": "passed", "evidence": evidence}
            self.assert_rejected(m, "103/R-cache [reference]: evidence must be a paired run: or report:")

    def test_report_naming_an_unknown_row_is_rejected(self):
        m = copy.deepcopy(MATRIX)
        m["rows"] = [r for r in m["rows"] if r["id"] != "75/75-S1"]
        self.assert_rejected(m, "names unknown matrix row 75/75-S1")

    def test_row_of_an_unknown_issue_is_rejected(self):
        m = copy.deepcopy(MATRIX)
        m["rows"].append({**copy.deepcopy(gate_row(m)), "id": "999/AC-1", "issue": 999})
        self.assert_rejected(m, "999/AC-1: issue 999 is not in the matrix issues")

    def test_row_whose_issue_field_is_unknown_is_rejected(self):
        m = copy.deepcopy(MATRIX)
        gate_row(m)["issue"] = 999
        self.assert_rejected(m, "issue 999 is not in the matrix issues")

    def test_epic_rows_stay_allowed(self):
        self.assertTrue([r for r in MATRIX["rows"] if r["id"].startswith("67/")])
        self.assertFalse([e for e in errors_for(copy.deepcopy(MATRIX)) if "67/" in e])

    def test_missing_evidence_doc_section(self):
        doc = EVIDENCE_DOC.replace("## Recorded differences", "## Something else")
        self.assert_rejected(copy.deepcopy(MATRIX), "lacks the 'Recorded differences' section",
                             doc=doc)

    def test_stale_markdown_view(self):
        self.assert_rejected(copy.deepcopy(MATRIX), "docs/compatibility-matrix.md is stale",
                             markdown=MARKDOWN + "\nedited by hand\n")


if __name__ == "__main__":
    unittest.main()
