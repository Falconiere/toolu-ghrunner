#!/usr/bin/env python3
"""compat_surfaces_check.evaluate against the recorded #237 capture and mutations of it."""

import copy
import importlib
import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
check = importlib.import_module("compat_surfaces_check")

ROOT = Path(__file__).resolve().parents[2]
CAPTURE = json.loads((ROOT / "docs/compat-surfaces-237.json").read_text())
TOOLU = "probe-Linux-X64-toolu"


def job(capture, scope, name):
    parsed = check.SCOPE.match(scope)
    wanted = f"{parsed['variant']}-{parsed['lane_input']} / {name}"
    return next(j for j in capture["jobs"] if j["name"] == wanted)


def failures(capture):
    return check.evaluate(capture)[1]


def annotations(capture, scope, name):
    return capture["annotations"][str(job(capture, scope, name)["id"])]


def status(capture, env, scope, name):
    job_id = job(capture, scope, name)["id"]
    return next(s for d in capture["deployments"][env] for s in d["statuses"]
                if s["state"] == "success" and s["job_id"] == job_id)


class RecordedCaptureTest(unittest.TestCase):
    def test_recorded_run_passes_every_scope(self):
        report, problems = check.evaluate(CAPTURE)
        self.assertEqual(problems, [])
        self.assertEqual(sorted(report["scopes"]), sorted(check.expected_scopes()))
        self.assertEqual(report["guard"], "success")

    def test_recorded_capture_holds_no_token_or_signed_url(self):
        self.assertEqual(check.leaks(CAPTURE), [])

    def test_accelerated_lane_ran_every_surface_without_docker_jobs(self):
        observed = check.evaluate(CAPTURE)[0]["scopes"]["probe-Linux-X64-toolu-accelerated"]
        self.assertEqual(sorted(observed["jobs"]),
                         ["annotations", "consumer", "deploy", "secrets", "services"])


class MismatchTest(unittest.TestCase):
    def setUp(self):
        self.capture = copy.deepcopy(CAPTURE)

    def assertFails(self, needle):
        found = failures(self.capture)
        self.assertTrue(any(needle in f for f in found), found)

    def test_wrong_severity_fails(self):
        error = next(a for a in annotations(self.capture, TOOLU, "annotations")
                     if a["message"] == "compat-237 error marker")
        error["annotation_level"] = "notice"
        self.assertFails(f"{TOOLU}: annotations:")

    def test_wrong_line_fails(self):
        warning = next(a for a in annotations(self.capture, TOOLU, "annotations")
                       if a["annotation_level"] == "warning")
        warning["start_line"] = 6
        self.assertFails(f"{TOOLU}: annotations:")

    def test_control_step_annotation_fails(self):
        extra = dict(annotations(self.capture, TOOLU, "annotations")[0],
                     message="compat-237 control line, not a workflow command")
        annotations(self.capture, TOOLU, "annotations").append(extra)
        self.assertFails("the control step produced an annotation")

    def test_missing_skip_output_warning_fails(self):
        items = annotations(self.capture, TOOLU, "secrets")
        items[:] = [a for a in items if not a["message"].startswith("Skip output")]
        self.assertFails(f"{TOOLU}: secrets: warnings")

    def test_secret_url_reaching_the_deployment_fails(self):
        status(self.capture, check.SECRET_ENV, TOOLU, "secrets")["environment_url"] = "https://example.com/x"
        self.assertFails(f"{TOOLU}: secrets: deployment environment_url")

    def test_wrong_safe_url_fails(self):
        status(self.capture, check.SAFE_ENV, TOOLU, "deploy")["environment_url"] = ""
        self.assertFails(f"{TOOLU}: deploy: deployment environment_url")

    def test_failed_oidc_step_fails(self):
        steps = job(self.capture, TOOLU, "services")["steps"]
        next(s for s in steps if s["name"] == "OIDC allowed")["conclusion"] = "failure"
        self.assertFails(f"{TOOLU}: services: step 'OIDC allowed'")

    def test_denied_oidc_step_is_required_in_the_negative_variant(self):
        scope = "negative-macOS-ARM64-toolu"
        steps = job(self.capture, scope, "services")["steps"]
        steps[:] = [s for s in steps if s["name"] != "OIDC denied without id-token permission"]
        self.assertFails(f"{scope}: services: step 'OIDC denied without id-token permission'")

    def test_failed_digest_step_fails(self):
        steps = job(self.capture, "probe-Linux-X64-toolu-accelerated", "consumer")["steps"]
        next(s for s in steps if s["name"] == "Verify outputs and artifact digests")["conclusion"] = "failure"
        self.assertFails("probe-Linux-X64-toolu-accelerated: consumer: step 'Verify outputs")

    def test_missing_container_job_fails_on_a_linux_toolu_lane(self):
        gone = job(self.capture, "probe-Linux-ARM64-toolu", "container")
        self.capture["jobs"].remove(gone)
        self.assertFails("probe-Linux-ARM64-toolu: container: job missing")

    def test_leaked_jwt_fails_without_quoting_it(self):
        jwt = "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJyZXBvIn0.x"
        status(self.capture, check.SAFE_ENV, TOOLU, "deploy")["environment_url"] = jwt
        found = failures(self.capture)
        self.assertTrue(any("leak in capture" in f for f in found), found)
        self.assertFalse(any("eyJ" in f for f in found), found)

    def test_signed_url_fails(self):
        annotations(self.capture, TOOLU, "secrets")[0]["message"] = "https://blob.example/x?sv=1&sig=abc"
        self.assertFails("leak in capture")

    def test_deployment_status_of_another_job_does_not_count(self):
        status(self.capture, check.SAFE_ENV, TOOLU, "deploy")["job_id"] = 1
        self.assertFails(f"{TOOLU}: deploy: deployment environment_url None")

    def test_failed_guard_fails(self):
        next(j for j in self.capture["jobs"] if j["name"] == "guard")["conclusion"] = "failure"
        self.assertFails("guard concluded failure")

    def test_official_lane_problem_is_recorded_not_failed(self):
        scope = "probe-Linux-X64-official"
        annotations(self.capture, scope, "annotations").clear()
        report, found = check.evaluate(self.capture)
        self.assertEqual(found, [])
        self.assertTrue(report["scopes"][scope]["problems"])


if __name__ == "__main__":
    unittest.main()
