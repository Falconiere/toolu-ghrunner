"""Check that issue 71 capture validation rejects altered real wire data."""

import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest

from defaults_run_capture_check import EVIDENCE, ROOT, check_capture


class CapturedDefaultsValidationTests(unittest.TestCase):
    def setUp(self):
        self.evidence = json.loads(EVIDENCE.read_text())
        fixture = ROOT / self.evidence['capture']['fixture']
        self.job = json.loads(fixture.read_text())

    def check_changed_capture_is_rejected(self, change):
        job = copy.deepcopy(self.job)
        change(job)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'job.json'
            path.write_text(json.dumps(job))
            evidence = copy.deepcopy(self.evidence)
            evidence['capture']['fixture'] = str(path)
            evidence['capture']['fixture_sha256'] = hashlib.sha256(path.read_bytes()).hexdigest()
            with self.assertRaises(AssertionError):
                check_capture(evidence)

    def test_real_sanitized_capture_passes(self):
        check_capture(self.evidence)

    def test_changed_secret_placeholder_is_rejected(self):
        self.check_changed_capture_is_rejected(
            lambda job: job['variables']['github_token'].update(value='[redacted]')
        )

    def test_wrong_defaults_token_kind_is_rejected(self):
        self.check_changed_capture_is_rejected(
            lambda job: job['defaults'][1].update(type=1)
        )


if __name__ == '__main__':
    unittest.main()
