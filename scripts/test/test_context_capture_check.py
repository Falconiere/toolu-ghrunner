"""Check sanitizer validation against the captured GitHub job messages."""
import unittest

from context_capture_check import FIXTURES, check_sanitized, read


class CapturedSanitizationTests(unittest.TestCase):
    def test_all_captured_slots_are_distinct_and_valid(self):
        evidence = read(FIXTURES / 'incoming_contexts_evidence.json')
        seen = set()
        for name in evidence['captures']:
            check_sanitized(name, read(FIXTURES / name), seen)
        self.assertEqual(len(seen), 81)

    def test_generic_marker_or_wrong_token_is_rejected(self):
        name = 'incoming_contexts_matrix_1.json'
        for replacement in ('[redacted]', '00000000-0000-0000-0000-000000000000'):
            with self.subTest(replacement=replacement):
                job = read(FIXTURES / name)
                job['variables']['github_token']['value'] = replacement
                with self.assertRaises(AssertionError):
                    check_sanitized(name, job, set())

    def test_secret_flag_is_preserved(self):
        name = 'incoming_contexts_call.json'
        job = read(FIXTURES / name)
        job['variables']['system.github.token']['isSecret'] = False
        with self.assertRaises(AssertionError):
            check_sanitized(name, job, set())


if __name__ == '__main__':
    unittest.main()
