"""Real captured-job provenance checks for issue 102 evidence."""

import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import composite_semantics_evidence_check as checker


EVIDENCE = checker.ROOT / 'crates/execution/tests/composite_semantics_evidence.json'


class CompositeSemanticsEvidenceTest(unittest.TestCase):
    def test_real_capture_and_committed_files_match_recorded_hashes(self):
        checker.check_capture(json.loads(EVIDENCE.read_text()))

    def test_changed_capture_hash_is_rejected(self):
        evidence = copy.deepcopy(json.loads(EVIDENCE.read_text()))
        evidence['capture']['sha256'] = '0' * 64
        with self.assertRaises(AssertionError):
            checker.check_capture(evidence)

    def test_unverified_live_lanes_cannot_be_claimed_as_complete(self):
        evidence = json.loads(EVIDENCE.read_text())
        evidence['runs']['toolu-macos'] = None
        evidence['unverified']['toolu-macos'] = 'fixture lane intentionally left unverified'
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'evidence.json'
            path.write_text(json.dumps(evidence))
            result = subprocess.run(
                [sys.executable, str(Path(checker.__file__)), str(path), '--require-live'],
                capture_output=True, text=True, check=False,
            )
        self.assertEqual(result.returncode, 2)
        self.assertIn('unverified', result.stdout + result.stderr)
        self.assertNotIn('Traceback', result.stderr)

    def test_extra_live_lane_reports_a_cli_error_without_traceback(self):
        evidence = json.loads(EVIDENCE.read_text())
        evidence['runs']['unexpected-lane'] = None
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'evidence.json'
            path.write_text(json.dumps(evidence))
            result = subprocess.run(
                [sys.executable, str(Path(checker.__file__)), str(path)],
                capture_output=True, text=True, check=False,
            )
        self.assertEqual(result.returncode, 2)
        self.assertIn('runs keys', result.stderr)
        self.assertNotIn('Traceback', result.stderr)

    def test_missing_evidence_reports_a_cli_error_without_traceback(self):
        with tempfile.TemporaryDirectory() as temp:
            missing = Path(temp) / 'missing.json'
            result = subprocess.run(
                [sys.executable, str(Path(checker.__file__)), str(missing)],
                capture_output=True, text=True, check=False,
            )
        self.assertEqual(result.returncode, 2)
        self.assertIn('cannot read evidence', result.stderr)
        self.assertNotIn('Traceback', result.stderr)

    def test_malformed_evidence_reports_a_cli_error_without_traceback(self):
        with tempfile.TemporaryDirectory() as temp:
            malformed = Path(temp) / 'malformed.json'
            malformed.write_text('{')
            result = subprocess.run(
                [sys.executable, str(Path(checker.__file__)), str(malformed)],
                capture_output=True, text=True, check=False,
            )
        self.assertEqual(result.returncode, 2)
        self.assertIn('cannot read evidence', result.stderr)
        self.assertNotIn('Traceback', result.stderr)


if __name__ == '__main__':
    unittest.main()
