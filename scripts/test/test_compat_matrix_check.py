#!/usr/bin/env python3
"""compat_matrix_check.validate against the committed matrix and mutations of it."""

import copy
import importlib
import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
check = importlib.import_module("compat_matrix_check")

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

    def test_report_evidence_only_backs_the_reference_lane(self):
        m = copy.deepcopy(MATRIX)
        gate_row(m)["lanes"]["github-com"] = {"status": "passed",
                                              "evidence": "report:docs/compat-reference-103.json"}
        self.assert_rejected(m, "'report:' evidence only backs the reference lane")

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
        row(m, "94/AC-1")["lanes"]["hermetic"] = {"status": "passed", "evidence": "gate"}
        self.assert_rejected(m, "open issue rows must be pending with the issue link")

    def test_acknowledged_reference_difference_cannot_pass(self):
        m = copy.deepcopy(MATRIX)
        row(m, "103/R-cache")["lanes"]["reference"] = {
            "status": "passed", "evidence": "report:docs/compat-reference-103.json"}
        self.assert_rejected(m, "records an unfixed difference; reference cannot pass")

    def test_missing_evidence_doc_section(self):
        doc = EVIDENCE_DOC.replace("## Recorded differences", "## Something else")
        self.assert_rejected(copy.deepcopy(MATRIX), "lacks the 'Recorded differences' section",
                             doc=doc)

    def test_stale_markdown_view(self):
        self.assert_rejected(copy.deepcopy(MATRIX), "docs/compatibility-matrix.md is stale",
                             markdown=MARKDOWN + "\nedited by hand\n")


if __name__ == "__main__":
    unittest.main()
