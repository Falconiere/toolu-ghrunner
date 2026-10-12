#!/usr/bin/env python3
"""sanitize_capture.py against a real committed acquire body with injected probe secrets."""

import copy
import importlib
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sanitize_capture = importlib.import_module("sanitize_capture")

ROOT = Path(__file__).resolve().parents[2]
CAPTURE = ROOT / "crates/execution/tests/job_outputs_message.json"

# Disposable probe values shaped like the real credentials a raw capture holds.
PROBE_TOKEN = "ghs_" + "0123456789abcdefABCDEF0123456789ab"
# Assembled at runtime so secret scanners reading this source see no JWT literal.
PROBE_JWT = ".".join(["eyJhbGciOiJSUzI1NiJ9", "eyJzdWIiOiJwcm9iZSJ9", "c2lnbmF0dXJlLXByb2Jl"])
PROBE_PLAIN = "plain-secret-probe-103"


def script_entry(step):
    """The captured `script` input token (wire shape: map of Key/Value)."""
    return next(e["Value"] for e in step["inputs"]["map"] if e["Key"]["lit"] == "script")


def raw_capture():
    """The real envelope with its placeholders swapped back for probe secrets."""
    message = json.loads(CAPTURE.read_text())
    message["variables"]["github_token"]["value"] = PROBE_TOKEN
    message["variables"]["system.github.token"]["value"] = PROBE_PLAIN
    message["mask"][0]["value"] = PROBE_TOKEN
    message["mask"][1]["value"] = PROBE_PLAIN
    endpoint = message["resources"]["endpoints"][0]
    endpoint["authorization"]["parameters"]["AccessToken"] = PROBE_JWT
    # A secret embedded inside a non-secret string (a step script).
    step = message["steps"][0]
    script = script_entry(step)
    script["lit"] = f"echo {PROBE_PLAIN} >/dev/null\n" + script["lit"]
    return message


def shape(node):
    """Structure without string contents: keys, order, types, lengths."""
    if isinstance(node, dict):
        return [(key, shape(value)) for key, value in node.items()]
    if isinstance(node, list):
        return [shape(value) for value in node]
    return type(node).__name__


def strings(node):
    return [text for _, text in sanitize_capture.walk(node)]


class SanitizeCaptureTest(unittest.TestCase):
    def test_every_probe_secret_is_replaced_everywhere(self):
        clean, count = sanitize_capture.sanitize(raw_capture(), "unit")
        joined = "\n".join(strings(clean))
        for probe in (PROBE_TOKEN, PROBE_JWT, PROBE_PLAIN):
            self.assertNotIn(probe, joined)
        self.assertGreaterEqual(count, 3)
        self.assertEqual(sanitize_capture.scan(clean), [])

    def test_equal_secrets_share_one_placeholder(self):
        clean, _ = sanitize_capture.sanitize(raw_capture(), "unit")
        token = clean["variables"]["github_token"]["value"]
        self.assertTrue(sanitize_capture.is_placeholder(token))
        self.assertEqual(clean["mask"][0]["value"], token)
        plain = clean["variables"]["system.github.token"]["value"]
        self.assertEqual(clean["mask"][1]["value"], plain)
        script = script_entry(clean["steps"][0])
        self.assertTrue(script["lit"].startswith(f"echo {plain} >/dev/null\n"))

    def test_structure_and_non_secret_values_are_preserved(self):
        raw = raw_capture()
        clean, _ = sanitize_capture.sanitize(copy.deepcopy(raw), "unit")
        self.assertEqual(shape(clean), shape(raw))
        for key in ("jobId", "requestId", "jobName", "fileTable", "contextData", "plan"):
            self.assertEqual(clean[key], raw[key], key)
        self.assertEqual([s["id"] for s in clean["steps"]], [s["id"] for s in raw["steps"]])
        self.assertEqual([s.get("contextName") for s in clean["steps"]],
                         [s.get("contextName") for s in raw["steps"]])
        self.assertEqual(clean["resources"]["endpoints"][0]["url"],
                         raw["resources"]["endpoints"][0]["url"])
        self.assertEqual(clean["resources"]["endpoints"][0]["authorization"]["parameters"],
                         {"AccessToken": "redacted"})
        data = clean["resources"]["endpoints"][0].get("data") or {}
        for url in data.values():
            if isinstance(url, str) and url.startswith("https://"):
                self.assertIsNone(sanitize_capture.OPAQUE_SEGMENT.search(url), url)

    def test_placeholders_do_not_depend_on_the_secret_value(self):
        first, _ = sanitize_capture.sanitize(raw_capture(), "unit")
        other = raw_capture()
        other["variables"]["github_token"]["value"] = "ghs_" + "Z" * 36
        other["mask"][0]["value"] = "ghs_" + "Z" * 36
        second, _ = sanitize_capture.sanitize(other, "unit")
        self.assertEqual(first["variables"]["github_token"]["value"],
                         second["variables"]["github_token"]["value"])

    def test_scan_rejects_the_raw_capture(self):
        problems = sanitize_capture.scan(raw_capture())
        self.assertTrue(any("github_token" in p for p in problems), problems)
        self.assertTrue(any("credential-shaped" in p for p in problems), problems)
        self.assertTrue(any("AccessToken is not redacted" in p for p in problems), problems)

    def test_cli_writes_a_clean_fixture_and_scan_accepts_it(self):
        with tempfile.TemporaryDirectory() as tmp:
            raw_path, out_path = Path(tmp, "raw.json"), Path(tmp, "out.json")
            raw_path.write_text(json.dumps(raw_capture()))
            self.assertEqual(sanitize_capture.main([str(raw_path), str(out_path)]), 0)
            self.assertEqual(sanitize_capture.main(["--scan", str(out_path)]), 0)
            self.assertEqual(sanitize_capture.main(["--scan", str(raw_path)]), 1)

    def test_committed_captures_are_clean(self):
        fixtures = [CAPTURE, *sorted(CAPTURE.parent.glob("compat_103_*_message.json"))]
        self.assertGreaterEqual(len(fixtures), 3)
        for fixture in fixtures:
            self.assertEqual(sanitize_capture.scan(json.loads(fixture.read_text())), [], fixture)

    def test_distinct_secrets_in_one_string_get_distinct_placeholders(self):
        raw = raw_capture()
        other = "ghs_" + "Y" * 36
        script = script_entry(raw["steps"][0])
        script["lit"] = f"echo {PROBE_TOKEN} {other}.\n" + script["lit"]
        clean, _ = sanitize_capture.sanitize(raw, "unit")
        first_line = script_entry(clean["steps"][0])["lit"].splitlines()[0]
        _, first, second = first_line.split()
        self.assertNotEqual(first, second.rstrip("."), first_line)
        self.assertNotIn(other, json.dumps(clean))


if __name__ == "__main__":
    unittest.main()
