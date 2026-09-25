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
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('unverified', result.stdout + result.stderr)


if __name__ == '__main__':
    unittest.main()
