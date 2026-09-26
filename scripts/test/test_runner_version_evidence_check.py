"""Check source binding using the recorded issue #78 live evidence and real files."""

import copy
import json
import unittest

from runner_version_evidence_check import ROOT, verify_source_hashes


class SourceEvidenceTest(unittest.TestCase):
    """Reject incomplete or stale evidence without mocking source files or GitHub."""

    def setUp(self):
        """Load the actual committed live-run metadata for each boundary variation."""
        self.evidence = json.loads((ROOT / "docs/runner-version-evidence.json").read_text())

    def test_recorded_source_matches_real_checkout(self):
        """The recorded live build includes hashes of all required source files."""
        verify_source_hashes(self.evidence)

    def test_empty_missing_extra_and_stale_source_hashes_fail(self):
        """A damaged copy of real evidence cannot silently omit version wiring."""
        for variation in ["empty", "missing", "extra", "stale"]:
            evidence = copy.deepcopy(self.evidence)
            hashes = evidence["source_sha256"]
            path = "crates/protocol/src/runner_version.rs"
            if variation == "empty":
                hashes.clear()
            elif variation == "missing":
                del hashes[path]
            elif variation == "extra":
                hashes["README.md"] = hashes[path]
            else:
                hashes[path] = "0" * 64
            with self.subTest(variation=variation), self.assertRaises(AssertionError):
                verify_source_hashes(evidence)


if __name__ == "__main__":
    unittest.main()
